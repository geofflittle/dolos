{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Writes the result and budget the ledger's machine gives each conformance program, for one language at one protocol version.
module Main (main) where

import qualified Codec.CBOR.Decoding as CBOR
import qualified Codec.CBOR.Encoding as CBOR
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Write as CBOR
import Control.Exception (ErrorCall (..), IOException, try)
import qualified Control.Exception as E
import Data.List (isPrefixOf)
import Control.Monad.Except (runExcept)
import Control.Monad.Writer (runWriterT)
import qualified Data.Aeson as A
import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Char8 as BC
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Short as SBS
import Data.Functor (void)
import Data.Int (Int64)
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.IO as T
import PlutusCore (DefaultFun, DefaultUni, Name)
import PlutusCore.DeBruijn (NamedDeBruijn, fakeNameDeBruijn, unNameDeBruijn)
import PlutusCore.Error (ParserErrorBundle)
import PlutusCore.Pretty (displayPlc, pretty)
import PlutusCore.Quote (runQuoteT)
import qualified PlutusLedgerApi.Common as P
import qualified PlutusLedgerApi.Common.Versions as Versions
import qualified PlutusLedgerApi.V3 as PV3
import qualified PlutusLedgerApi.V4 as PV4
import System.Environment (getArgs)
import qualified UntypedPlutusCore as UPLC
import PlutusCore.Evaluation.Machine.ExBudget (ExBudget (..), ExRestrictingBudget (..), minusExBudget)
import UntypedPlutusCore.Evaluation.Machine.Cek (CekReport (..), RestrictingSt (..), cekResultToEither, restricting)
import qualified UntypedPlutusCore.Parser as Parser

-- | The marker of a program the ledger cannot decode.
decodeError :: String
decodeError = "parse/decode error"

-- | The marker of a program whose evaluation fails.
evaluationFailure :: String
evaluationFailure = "evaluation failure"

-- | The units each program may spend, the transaction maximum.
limit :: ExBudget
limit = ExBudget 10000000000 14000000

-- | The builtins a term calls.
builtins :: UPLC.Term name DefaultUni DefaultFun ann -> [DefaultFun]
builtins term = case term of
  UPLC.Builtin _ f -> [f]
  UPLC.LamAbs _ _ body -> builtins body
  UPLC.Apply _ f a -> builtins f ++ builtins a
  UPLC.Force _ body -> builtins body
  UPLC.Delay _ body -> builtins body
  UPLC.Constr _ _ fields -> foldMap builtins fields
  UPLC.Case _ scrutinee branches -> builtins scrutinee ++ foldMap builtins branches
  _ -> []

-- | The output of one term and the units it spends, by the ledger's machine, or nothing when evaluation fails.
evaluate :: P.MajorProtocolVersion -> UPLC.Version -> P.EvaluationContext -> UPLC.Term NamedDeBruijn DefaultUni DefaultFun () -> Maybe (UPLC.Term NamedDeBruijn DefaultUni DefaultFun (), ExBudget)
evaluate pv version ctx term =
  let CekReport res (RestrictingSt (ExRestrictingBudget final)) _ = P.evaluateTerm (restricting (ExRestrictingBudget limit)) pv version P.Quiet ctx term
   in either (const Nothing) (\out -> Just (out, limit `minusExBudget` final)) (cekResultToEither res)

-- | The expected output and budget of one program text, by the ledger's version and builtin availability and its machine.
result :: P.PlutusLedgerLanguage -> P.MajorProtocolVersion -> P.EvaluationContext -> T.Text -> (String, String)
result ll pv ctx text = case runExcept (runQuoteT (Parser.parseProgram text)) of
  Left (_ :: ParserErrorBundle) -> (decodeError, decodeError)
  Right (UPLC.Program _ version body)
    | not (version `Set.member` Versions.plcVersionsAvailableIn ll pv) -> (decodeError, decodeError)
    | not (all (`Set.member` Versions.builtinsAvailableIn ll pv) (builtins body)) -> (decodeError, decodeError)
    | otherwise -> case UPLC.deBruijnTerm (void body) of
        Left (_ :: UPLC.FreeVariableError) -> (evaluationFailure, evaluationFailure)
        Right term -> case evaluate pv version ctx term of
          Nothing -> (evaluationFailure, evaluationFailure)
          Just (out, spent) -> (shown version out, show (pretty spent))
  where
    shown version out = case runExcept (runQuoteT (UPLC.unDeBruijnTerm (UPLC.termMapNames (fakeNameDeBruijn . unNameDeBruijn) out))) of
      Left (e :: UPLC.FreeVariableError) -> error (show e)
      Right (named :: UPLC.Term Name DefaultUni DefaultFun ()) -> displayPlc (UPLC.Program () version named)

-- | The expected output of one flat program in hex, by the ledger's script decoder, its term preparation and its machine.
flatResult :: P.PlutusLedgerLanguage -> P.MajorProtocolVersion -> P.EvaluationContext -> BS.ByteString -> String
flatResult ll pv ctx bytes = case runExcept (P.deserialiseScript ll pv (SBS.toShort (CBOR.toStrictByteString (CBOR.encodeBytes bytes)))) of
  Left (_ :: P.ScriptDecodeError) -> decodeError
  Right script -> case runExcept (P.mkTermToEvaluate ll pv script []) of
    Left (P.CodecError _) -> decodeError
    Left _ -> evaluationFailure
    Right term ->
      let P.ScriptNamedDeBruijn (UPLC.Program _ version _) = P.deserialisedScript script
       in case evaluate pv version ctx term of
            Nothing -> evaluationFailure
            Just (out, _) -> case CBOR.deserialiseFromBytes CBOR.decodeBytes (BL.fromStrict (SBS.fromShort (P.serialiseUPLC (UPLC.Program () version (UPLC.termMapNames unNameDeBruijn out))))) of
              Left e -> error (show e)
              Right (_, encoded) -> BC.unpack (B16.encode encoded)

-- | Reads the language, the major protocol version, the cost model file and the list of program paths, and writes each program's expected files.
main :: IO ()
main = do
  [language, major, costsPath, listPath] <- getArgs
  costs <- maybe (fail "the cost model is not a JSON list of integers") pure . A.decode =<< BL.readFile costsPath
  let pv = P.MajorProtocolVersion (read major)
      context :: Either P.CostModelApplyError P.EvaluationContext
      (ll, context) = case language of
        "V3" -> (P.PlutusV3, fst <$> runExcept (runWriterT (PV3.mkEvaluationContext (costs :: [Int64]))))
        "V4" -> (P.PlutusV4, fst <$> runExcept (runWriterT (PV4.mkEvaluationContext costs)))
        _ -> error ("no language " ++ language)
  ctx <- either (fail . show) pure context
  paths <- lines <$> readFile listPath
  mapM_ (write ll pv ctx) paths
  where
    write ll pv ctx path = do
      text <- T.readFile ("/probe/in/" ++ path)
      let (out, budget) = result ll pv ctx text
          base = take (length path - length (".uplc" :: String)) path
      writeFile ("/probe/out/" ++ path ++ ".expected") out
      writeFile ("/probe/out/" ++ base ++ ".budget") budget
      program <- try (BS.readFile ("/probe/in/" ++ base ++ ".flat"))
      case program of
        Left (_ :: IOException) -> pure ()
        Right hex -> case B16.decode (BC.filter (`notElem` (" \n" :: String)) hex) of
          Left e -> fail ("the flat program " ++ base ++ " is not hex: " ++ e)
          Right bytes -> do
            -- A BLS element output has no flat form, so its program has no flat expectation.
            out <- try (E.evaluate (force (flatResult ll pv ctx bytes)))
            case out of
              Left (ErrorCall message) | "Flat encoding is not supported" `isPrefixOf` message -> pure ()
              Left e -> E.throwIO e
              Right expected -> writeFile ("/probe/out/" ++ base ++ ".flat.expected") expected
    force s = length s `seq` s
