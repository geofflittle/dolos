{-# LANGUAGE OverloadedStrings #-}

-- | Writes the protocol 12 machine slice, one line per raw program with the result the ledger's script evaluation gives it.
module Main (main) where

import Cardano.Ledger.Plutus.CostModels (getCostModelParams, getEvaluationContext)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (Language (..))
import Cardano.Ledger.Plutus.TxInfo (transExUnits)
import Codec.Serialise (serialise)
import qualified Data.Aeson as A
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Lazy.Char8 as BL8
import qualified Data.ByteString.Short as SBS
import Dump (evalJson, hex)
import PlutusCore (DefaultFun (..), DefaultUni)
import qualified PlutusCore as PLC
import PlutusCore.DeBruijn (DeBruijn (..))
import PlutusCore.Version (plcVersion100, plcVersion110)
import qualified PlutusLedgerApi.Common as P
import qualified PlutusLedgerApi.V1 as PV1
import qualified PlutusLedgerApi.V2 as PV2
import qualified PlutusLedgerApi.V3 as PV3
import Test.Cardano.Ledger.Plutus (testingCostModel)
import qualified UntypedPlutusCore as UPLC

-- | A term of a program.
type T = UPLC.Term DeBruijn DefaultUni DefaultFun ()

-- | The units every program may spend, the transaction maximum.
budget :: ExUnits
budget = ExUnits 14000000 10000000000

-- | The smallest positive integer that does not fit in 262144 signed bits.
aboveBound :: Integer
aboveBound = 2 ^ (262143 :: Int)

-- | An integer constant.
int :: Integer -> T
int = UPLC.Constant () . PLC.someValue

-- | A bytestring constant.
bytes :: BS.ByteString -> T
bytes = UPLC.Constant () . PLC.someValue

-- | The unit constant.
unit :: T
unit = UPLC.Constant () (PLC.someValue ())

-- | A builtin applied to its arguments.
builtin :: DefaultFun -> [T] -> T
builtin f = foldl (UPLC.Apply ()) (UPLC.Builtin () f)

-- | A function of one argument.
lam :: T -> T
lam = UPLC.LamAbs () (DeBruijn 0)

-- | The innermost bound variable.
var :: T
var = UPLC.Var () (DeBruijn 1)

-- | Unit when the boolean term is true, otherwise an error.
check :: T -> T
check b =
  UPLC.Force () $
    foldl (UPLC.Apply ()) (UPLC.Force () (UPLC.Builtin () IfThenElse)) [b, UPLC.Delay () unit, UPLC.Delay () (UPLC.Error ())]

-- | Whether two integer terms are equal.
equal :: T -> T -> T
equal a b = builtin EqualsInteger [a, b]

-- | The programs of the slice, each named, with its language, its body and the data it is applied to.
programs :: [(String, Language, T, [P.Data])]
programs =
  [ ("v3-add", PlutusV3, lam (check (equal (builtin AddInteger [int 2, int 3]) (int 5))), ctx)
  , ("v3-divide-floor", PlutusV3, lam (check (equal (builtin DivideInteger [int (-7), int 2]) (int (-4)))), ctx)
  , ("v3-quotient", PlutusV3, lam (check (equal (builtin QuotientInteger [int (-7), int 2]) (int (-3)))), ctx)
  , ("v3-mod", PlutusV3, lam (check (equal (builtin ModInteger [int (-7), int 2]) (int 1))), ctx)
  , ("v3-multiply", PlutusV3, lam (check (builtin LessThanInteger [builtin MultiplyInteger [int (2 ^ (200 :: Int)), int 3], int (2 ^ (202 :: Int))])), ctx)
  , ("v3-bytes", PlutusV3, lam (check (equal (builtin LengthOfByteString [builtin AppendByteString [bytes "\1", bytes "\2\3"]]) (int 3))), ctx)
  , ("v3-blake2b", PlutusV3, lam (check (equal (builtin LengthOfByteString [builtin Blake2b_256 [bytes "abc"]]) (int 32))), ctx)
  , ("v3-argument", PlutusV3, lam (check (equal (builtin UnIData [var]) (int 7))), ctx)
  , ("v3-error", PlutusV3, lam (UPLC.Error ()), ctx)
  , ("v3-not-unit", PlutusV3, lam (int 1), ctx)
  , ("v3-unaudited-builtin", PlutusV3, lam (check (equal (builtin LengthOfByteString [builtin Sha2_256 [bytes "abc"]]) (int 32))), ctx)
  , ("v3-integer-at-bound", PlutusV3, lam (check (builtin LessThanInteger [int 1, int (aboveBound - 1)])), ctx)
  , ("v3-integer-above-bound", PlutusV3, lam (check (builtin LessThanInteger [int 1, int aboveBound])), ctx)
  , ("v1-divide", PlutusV1, lam (lam (lam (builtin DivideInteger [int (-7), int 2]))), legacy)
  , ("v1-error", PlutusV1, lam (lam (lam (UPLC.Error ()))), legacy)
  , ("v2-mod", PlutusV2, lam (lam (lam (builtin ModInteger [int (-7), int 2]))), legacy)
  ]
  where
    ctx = [P.I 7]
    legacy = [P.I 1, P.I 2, P.Constr 0 []]

-- | Decodes and evaluates a script of a language at protocol 12 with the ledger's own functions for that language.
evaluate :: Language -> SBS.ShortByteString -> [P.Data] -> A.Value
evaluate lang script args = case lang of
  PlutusV1 -> either undecodable (\s -> evalJson (PV1.evaluateScriptRestricting pv P.Verbose context limit s args)) (PV1.deserialiseScript pv script)
  PlutusV2 -> either undecodable (\s -> evalJson (PV2.evaluateScriptRestricting pv P.Verbose context limit s args)) (PV2.deserialiseScript pv script)
  PlutusV3 -> case args of
    [arg] -> either undecodable (\s -> evalJson (PV3.evaluateScriptRestricting pv P.Verbose context limit s arg)) (PV3.deserialiseScript pv script)
    _ -> error "a PlutusV3 program takes one argument"
  PlutusV4 -> error "no PlutusV4 program in the slice"
  where
    pv = P.MajorProtocolVersion 12
    context = getEvaluationContext (testingCostModel lang)
    limit = transExUnits budget
    undecodable e = A.object ["undecodable" A..= show e]

-- | The line of one program.
row :: (String, Language, T, [P.Data]) -> A.Value
row (name, lang, body, args) =
  let version = if lang == PlutusV3 then plcVersion110 else plcVersion100
      script = P.serialiseUPLC (UPLC.Program () version body)
      ExUnits mem steps = budget
   in A.object
        [ "name" A..= name
        , "language" A..= show lang
        , "protocol_version" A..= (12 :: Int)
        , "script" A..= hex (SBS.fromShort script)
        , "args" A..= map (hex . BL.toStrict . serialise) args
        , "budget" A..= [mem, steps]
        , "cost_model" A..= getCostModelParams (testingCostModel lang)
        , "result" A..= evaluate lang script args
        ]

-- | Writes the line of each program.
main :: IO ()
main = BL8.writeFile "/probe/out/uplc.jsonl" (BL8.unlines (map (A.encode . row) programs))
