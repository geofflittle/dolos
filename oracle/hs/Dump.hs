{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE NamedFieldPuns #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | One JSON line per LEDGER rule run, written from the Imp post submit hook.
module Dump (E, decodeTx, dumpTo, editedBytes, failurePath) where

import Cardano.Ledger.Alonzo.PParams (ppCostModelsL, ppMaxTxExUnitsL)
import Cardano.Ledger.Alonzo.Plutus.Context (LedgerLevelTxInfo (..), LedgerTxInfo (..), SupportedPlutusRunnable (..), toScriptHashByPurpose)
import Cardano.Ledger.Alonzo.Plutus.Evaluate (evalTxExUnits)
import Cardano.Ledger.Alonzo.Plutus.TxInfo (mkPlutusWithContext)
import Cardano.Ledger.Alonzo.Scripts (AsIx, PlutusPurpose, hoistPlutusPurpose, toAsIx)
import Cardano.Ledger.Alonzo.Tx (IsPhase2Valid (..), isPhase2ValidTxL)
import Cardano.Ledger.Alonzo.TxWits (rdmrsTxWitsL, unRedeemersL)
import Cardano.Ledger.Alonzo.UTxO (resolveNeededPlutusScriptsWithPurpose)
import Cardano.Ledger.BaseTypes (Globals (..), Network, TxIx (..), epochInfoPure, pvMajor, unNonZero)
import Cardano.Ledger.Binary (DecCBOR (decCBOR), DecoderError, Version, decodeFullAnnotator, serialize')
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Core
import Cardano.Ledger.Dijkstra (DijkstraEra)
import Cardano.Ledger.Dijkstra.Tx (DijkstraStAnnTx (..))
import Cardano.Ledger.Plutus.CostModels (costModelsValid)
import Cardano.Ledger.Plutus.Evaluate (evaluatePlutusWithContext)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.TxInfo (exBudgetToExUnits)
import Cardano.Ledger.Plutus.Language (plutusLanguage)
import Cardano.Ledger.Shelley.LedgerState (lsCertStateL, lsUTxOStateL)
import Cardano.Ledger.Shelley.Rules (LedgerEnv (..))
import Cardano.Ledger.State (UTxO (..), accountsL, certDStateL, getMinFeeTxUtxo, utxoL)
import Cardano.Slotting.EpochInfo (epochInfoSize)
import Cardano.Slotting.Slot (EpochNo (..), EpochSize (..), SlotNo (..))
import Control.Monad.IO.Class (liftIO)
import Control.State.Transition.Extended (STS (..), TRC (..))
import qualified Data.Aeson as A
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Char8 as BS8
import qualified Data.ByteString.Lazy.Char8 as BL8
import Data.Char (isAlphaNum, isSpace, isUpper)
import Data.Functor.Identity (runIdentity)
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (dropWhileEnd, isSuffixOf)
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import Lens.Micro ((^.))
import qualified PlutusLedgerApi.Common as P
import System.IO.Unsafe (unsafePerformIO)
import Test.Cardano.Ledger.Dijkstra.ImpTest

type E = DijkstraEra

{-# NOINLINE lastPost #-}
lastPost :: IORef (Maybe (UTxO E))
lastPost = unsafePerformIO (newIORef Nothing)

{-# NOINLINE editedBytes #-}
-- | The bytes the next record carries in place of its transaction's encoding, which the node's decoder refuses.
editedBytes :: IORef (Maybe BS8.ByteString)
editedBytes = unsafePerformIO (newIORef Nothing)

{-# NOINLINE counter #-}
counter :: IORef Int
counter = unsafePerformIO (newIORef 0)

hex :: BS8.ByteString -> String
hex = BS8.unpack . B16.encode

pairs :: UTxO E -> [[String]]
pairs (UTxO m) =
  [ [hex (serialize' (eraProtVerHigh @E) i), hex (serialize' (eraProtVerHigh @E) o)]
  | (i, o) <- Map.toList m
  ]

-- | The transaction the node's decoder reads from the bytes at a protocol version.
decodeTx :: Version -> BS8.ByteString -> Either DecoderError (Tx TopTx E)
decodeTx v b = decodeFullAnnotator v "Tx" decCBOR (BL8.fromStrict b)

-- | The inputs the first UTxO holds and the second does not.
consumed :: UTxO E -> UTxO E -> [String]
consumed (UTxO before) (UTxO after) =
  [hex (serialize' (eraProtVerHigh @E) i) | i <- Map.keys (Map.difference before after)]

-- | The entries of the second UTxO that the first does not hold with the same output.
produced :: UTxO E -> UTxO E -> UTxO E
produced (UTxO before) (UTxO after) =
  UTxO (Map.differenceWith (\new old -> if new == old then Nothing else Just new) after before)

-- | The constructor names of one shown failure, from the outer wrapper to the leaf.
failurePath :: String -> [String]
failurePath s = case dropWhile isSpace s of
  c : cs
    | isUpper c ->
        let (rest, after) = span isIdent cs
            name = c : rest
         in name : if "Failure" `isSuffixOf` name then maybe [] failurePath (argument (trim after)) else []
  _ -> []
  where
    isIdent x = isAlphaNum x || x == '_' || x == '\''
    trim = dropWhileEnd isSpace . dropWhile isSpace
    argument a = case soleGroup a of
      Just inner -> Just inner
      Nothing -> case a of
        x : xs | isUpper x && all isIdent xs -> Just a
        _ -> Nothing

-- | The inside of a parenthesised group that spans the whole string.
soleGroup :: String -> Maybe String
soleGroup ('(' : xs) = go (0 :: Int) xs ""
  where
    go 0 [')'] acc = Just (reverse acc)
    go 0 (')' : _) _ = Nothing
    go d (')' : ys) acc = go (d - 1) ys (')' : acc)
    go d ('(' : ys) acc = go (d + 1) ys ('(' : acc)
    go d ('"' : ys) acc = let (lit, rest) = stringLit ys in go d rest (reverse ('"' : lit) ++ acc)
    go d (y : ys) acc = go d ys (y : acc)
    go _ [] _ = Nothing
soleGroup _ = Nothing

-- | A shown string literal after its opening quote, split after its closing quote.
stringLit :: String -> (String, String)
stringLit ('\\' : y : ys) = let (lit, rest) = stringLit ys in ('\\' : y : lit, rest)
stringLit ('"' : ys) = ("\"", ys)
stringLit (y : ys) = let (lit, rest) = stringLit ys in (y : lit, rest)
stringLit [] = ([], [])

globalsJson :: Globals -> EpochNo -> A.Value
globalsJson g e =
  A.object
    [ "network" A..= show (networkId g :: Network)
    , "epoch_size" A..= unEpochSize (runIdentity (epochInfoSize (epochInfoPure g) e))
    , "security_param" A..= unNonZero (securityParameter g)
    ]

dumpTo ::
  FilePath ->
  Globals ->
  TRC (EraRule "LEDGER" E) ->
  Either (NE.NonEmpty (PredicateFailure (EraRule "LEDGER" E))) (State (EraRule "LEDGER" E), [Event (EraRule "LEDGER" E)]) ->
  ImpM t ()
dumpTo out g (TRC (env, st, stAnn)) res = liftIO $ do
  n <- readIORef counter
  writeIORef counter (n + 1)
  prev <- readIORef lastPost
  edited <- readIORef editedBytes
  writeIORef editedBytes Nothing
  let tx = stAnn ^. txStAnnTxG
      pre = st ^. lsUTxOStateL . utxoL
      epoch = ledgerEpochNo env
      valid = tx ^. isPhase2ValidTxL == Phase2Valid
      bytes = fromMaybe (serialize' (eraProtVerHigh @E) tx) edited
      accountsJson s = A.toJSON (s ^. lsCertStateL . certDStateL . accountsL)
      refused shown names =
        A.object ["refused" A..= A.object ["failures" A..= take 300 shown, "failure_names" A..= names]]
      decoded = decodeTx (pvMajor (ledgerPp env ^. ppProtocolVersionL)) bytes
      (outcome, post) = case (decoded, res) of
        (Left e, _) -> (refused (show e) [failurePath (show e)], Nothing)
        (Right _, Left fs) -> (refused (show (NE.toList fs)) (map (failurePath . show) (NE.toList fs)), Nothing)
        (Right _, Right (st', _)) ->
          let p = st' ^. lsUTxOStateL . utxoL
           in ( A.object
                  [ "accepted"
                      A..= A.object
                        [ "consumed" A..= consumed pre p
                        , "produced" A..= pairs (produced pre p)
                        , "accounts_post" A..= accountsJson st'
                        ]
                  ]
              , Just p
              )
      line =
        A.object $
          [ "n" A..= n
          , "slot" A..= unSlotNo (ledgerSlotNo env)
          , "epoch" A..= fmap unEpochNo epoch
          , "phase2_valid" A..= valid
          , "tx" A..= hex bytes
          , "pparams" A..= A.toJSON (ledgerPp env)
          , "pre" A..= if prev == Just pre then A.String "follows_prev" else A.object ["fresh" A..= pairs pre]
          , "accounts_pre" A..= accountsJson st
          , "outcome" A..= outcome
          , "ex_units" A..= map evaluationJson (Map.toList (evalTxExUnits (ledgerPp env) tx pre (epochInfo g) (systemStart g)))
          , "sub_ex_units" A..= map (map evaluationJson . Map.toList) (subTxExUnits (ledgerPp env) g pre stAnn)
          , "min_fee" A..= either (const Nothing) (\t -> Just (unCoin (getMinFeeTxUtxo (ledgerPp env) t pre))) decoded
          ]
            ++ [ "globals" A..= globalsJson g e | n == 0, Just e <- [epoch] ]
  writeIORef lastPost (maybe (Just pre) Just post)
  BL8.appendFile out (A.encode line <> "\n")
  where
    evaluationJson :: Show e => (PlutusPurpose AsIx E, Either e ExUnits) -> A.Value
    evaluationJson (purpose, result) =
      A.object
        [ "purpose" A..= hex (serialize' (eraProtVerHigh @E) purpose)
        , "evaluation" A..= case result of
            Right (ExUnits mem steps) -> A.object ["ran" A..= [mem, steps]]
            Left failure -> A.object ["failed" A..= take 300 (show failure)]
        ]

-- | The units each sub transaction's redeemers run in at the maximum budget, one map per sub transaction in order, evaluated in that sub transaction's own context.
subTxExUnits :: PParams E -> Globals -> UTxO E -> DijkstraStAnnTx TopTx E -> [Map.Map (PlutusPurpose AsIx E) (Either String ExUnits)]
subTxExUnits pp g utxo DijkstraStAnnTopTx {dsattSubTransactions} = zipWith one [0 ..] dsattSubTransactions
  where
    protVer = pp ^. ppProtocolVersionL
    costModels = costModelsValid (pp ^. ppCostModelsL)
    one :: Int -> DijkstraStAnnTx SubTx E -> Map.Map (PlutusPurpose AsIx E) (Either String ExUnits)
    one ix DijkstraStAnnSubTx {dsastTx, dsastScriptsNeeded, dsastScriptsProvided, dsastTxInfoResult} =
      Map.mapWithKey run (dsastTx ^. witsTxL . rdmrsTxWitsL . unRedeemersL)
      where
        (_, used) = resolveNeededPlutusScriptsWithPurpose protVer dsastScriptsProvided dsastScriptsNeeded mempty
        byPointer = Map.fromList [(hoistPlutusPurpose toAsIx purpose, (purpose, runnable)) | (purpose, runnable) <- used]
        lti =
          LedgerTxInfo
            { ltiProtVer = protVer
            , ltiEpochInfo = epochInfo g
            , ltiSystemStart = systemStart g
            , ltiUTxO = utxo
            , ltiTx = dsastTx
            , ltiScriptsUsed = used
            , ltiScriptHashesUsed = toScriptHashByPurpose used
            , ltiLevelTxInfo = LedgerSubTxInfo (TxIx (fromIntegral ix))
            }
        run pointer (redeemerData, _) = do
          (purpose, runnable) <- maybe (Left ("no script for " <> show pointer)) Right (Map.lookup pointer byPointer)
          let lang = case runnable of SupportedPlutusRunnable r -> plutusLanguage r
          costModel <- maybe (Left ("no cost model for " <> show lang)) Right (Map.lookup lang costModels)
          pwc <- either (Left . show) Right $ mkPlutusWithContext runnable purpose lti dsastTxInfoResult redeemerData (pp ^. ppMaxTxExUnitsL) costModel
          case evaluatePlutusWithContext P.Verbose pwc of
            (_, Left failure) -> Left (show failure)
            (_, Right budget) -> maybe (Left ("incompatible budget " <> show budget)) Right (exBudgetToExUnits budget)
