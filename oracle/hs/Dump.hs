{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NamedFieldPuns #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | One JSON line per LEDGER rule run, written from the Imp post submit hook.
module Dump (E, decodeTx, dumpTo, editedBytes, failurePath) where

import Cardano.Ledger.Alonzo.PParams (ppCostModelsL, ppMaxTxExUnitsL)
import Cardano.Ledger.Alonzo.Plutus.Context (EraPlutusContext (..), LedgerLevelTxInfo (..), LedgerTxInfo (..), toScriptHashByPurpose)
import Cardano.Ledger.Alonzo.Plutus.Evaluate (RedeemerReportWithLogs, TransactionScriptFailure (..))
import Cardano.Ledger.Alonzo.Plutus.TxInfo (mkPlutusWithContext)
import Cardano.Ledger.Alonzo.Scripts (AsIx, PlutusPurpose, hoistPlutusPurpose, lookupPlutusScript, plutusScriptLanguage, toAsItem, toAsIx)
import Cardano.Ledger.Alonzo.Tx (IsPhase2Valid (..), isPhase2ValidTxL)
import Cardano.Ledger.Alonzo.TxWits (rdmrsTxWitsL, unRedeemersL)
import Cardano.Ledger.Alonzo.UTxO (AlonzoScriptsNeeded (..), resolveNeededPlutusScriptsWithPurpose)
import Cardano.Ledger.BaseTypes (Globals (..), TxIx (..), activeSlotVal, epochInfoPure, pvMajor, unNonZero, unboundRational)
import Cardano.Ledger.Binary (DecCBOR (decCBOR), DecoderError, Version, decodeFullAnnotator, serialize')
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Core
import Cardano.Ledger.Dijkstra (DijkstraEra)
import Cardano.Ledger.Dijkstra.Tx (DijkstraStAnnTx (..))
import Cardano.Ledger.Plutus.CostModels (costModelsValid)
import Cardano.Ledger.Plutus.Evaluate (PlutusWithContext (..), evaluatePlutusWithContext)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (LegacyPlutusArgs (..), PlutusArgs (..), PlutusLanguage (..), PlutusScriptContext, SLanguage (..))
import Cardano.Ledger.Plutus.TxInfo (exBudgetToExUnits)
import Cardano.Ledger.Conway.Governance (GovRelation (..), PRoot (..), cgsCommitteeL, cgsConstitutionL, cgsProposalsL, pRootsL)
import Cardano.Ledger.Shelley.LedgerState (lsCertStateL, lsUTxOStateL, utxosGovStateL)
import Cardano.Ledger.Shelley.Rules (LedgerEnv (..))
import qualified Data.Aeson.KeyMap as KM
import Cardano.Ledger.State (ChainAccountState (..), EraUTxO (..), ScriptsProvided (..), UTxO (..), accountsL, certDStateL, getMinFeeTxUtxo, utxoL)
import Cardano.Slotting.EpochInfo (epochInfoEpoch, epochInfoSize, epochInfoSlotLength, epochInfoSlotToRelativeTime)
import Cardano.Slotting.Slot (EpochNo (..), EpochSize (..), SlotNo (..))
import Cardano.Slotting.Time (SystemStart (..), slotLengthToMillisec)
import Codec.Serialise (serialise)
import Data.Bifunctor (first)
import Data.Either (isRight)
import Data.MapExtras (fromElems)
import Data.Ratio (denominator, numerator)
import Data.Time.Clock.POSIX (utcTimeToPOSIXSeconds)
import Control.Monad (when)
import Control.Monad.IO.Class (liftIO)
import Control.State.Transition.Extended (STS (..), TRC (..))
import qualified Data.Aeson as A
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Char8 as BS8
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Lazy.Char8 as BL8
import Data.Char (isAlphaNum, isSpace, isUpper)
import Data.Functor.Identity (runIdentity)
import Data.IORef (IORef, modifyIORef', newIORef, readIORef, writeIORef)
import Data.List (dropWhileEnd, isSuffixOf)
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import Data.Text (Text)
import Cardano.Ledger.Shelley.API.Mempool (ApplyTx (mkStAnnTx))
import Control.Monad.Reader (runReader)
import Control.State.Transition.Extended (applySTS)
import Lens.Micro ((&), (.~), (^.))
import qualified PlutusLedgerApi.Common as P
import qualified PlutusLedgerApi.V3 as PV3
import System.IO (hPutStrLn, stderr)
import System.IO.Unsafe (unsafePerformIO)
import Test.Cardano.Ledger.Dijkstra.ImpTest hiding (PlutusArgs)

type E = DijkstraEra

{-# NOINLINE lastPost #-}
lastPost :: IORef (Maybe (UTxO E, A.Value, A.Value))
lastPost = unsafePerformIO (newIORef Nothing)

{-# NOINLINE editedBytes #-}
-- | The bytes the next record carries in place of its transaction's encoding, which the node's decoder refuses.
editedBytes :: IORef (Maybe BS8.ByteString)
editedBytes = unsafePerformIO (newIORef Nothing)

{-# NOINLINE counter #-}
counter :: IORef Int
counter = unsafePerformIO (newIORef 0)

{-# NOINLINE ruleKeys #-}
-- | The number of redeemers so far whose dumped arguments were compared with the arguments the rule built.
ruleKeys :: IORef Int
ruleKeys = unsafePerformIO (newIORef 0)

hex :: BS8.ByteString -> String
hex = BS8.unpack . B16.encode

-- | The data arguments a script is applied to, in order.
argsData :: forall l. PlutusLanguage l => PlutusArgs l -> [P.Data]
argsData args = case (isLanguage :: SLanguage l) of
  SPlutusV1 -> legacy (unPlutusV1Args args)
  SPlutusV2 -> legacy (unPlutusV2Args args)
  SPlutusV3 -> [PV3.toData (unPlutusV3Args args)]
  SPlutusV4 -> [PV3.toData (unPlutusV4Args args)]
  where
    legacy :: PV3.ToData (PlutusScriptContext k) => LegacyPlutusArgs k -> [P.Data]
    legacy = \case
      LegacyPlutusArgs2 r c -> [r, PV3.toData c]
      LegacyPlutusArgs3 d r c -> [d, r, PV3.toData c]

-- | The data arguments of a script with its context.
pwcData :: PlutusWithContext -> [P.Data]
pwcData PlutusWithContext {pwcArgs} = argsData pwcArgs

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

-- | The slot to time map the ledger reads at the slot, and the first slot it cannot translate when one is within two epochs past the stability window.
timeJson :: Globals -> SlotNo -> A.Value
timeJson g s =
  A.object
    [ "era"
        A..= A.object
          [ "system_start_ms" A..= startMs
          , "slot_length_ms" A..= slotLengthToMillisec (runIdentity (epochInfoSlotLength fixed s))
          , "epoch_size" A..= size
          , "security_param" A..= unNonZero (securityParameter g)
          , "active_slot_coeff" A..= [numerator f, denominator f]
          ]
    , "horizon_slot" A..= horizon
    ]
  where
    fixed = epochInfoPure g
    SystemStart start = systemStart g
    startMs = floor (utcTimeToPOSIXSeconds start * 1000) :: Integer
    f = unboundRational (activeSlotVal (activeSlotCoeff g))
    size = unEpochSize (runIdentity (epochInfoEpoch fixed s >>= epochInfoSize fixed))
    translates x = isRight (epochInfoSlotToRelativeTime (epochInfo g) (SlotNo x))
    top = unSlotNo s + stabilityWindow g + 2 * size
    horizon = if translates top then Nothing else Just (firstPast (unSlotNo s) top)
    firstPast lo hi
      | lo >= hi = hi
      | translates mid = firstPast (mid + 1) hi
      | otherwise = firstPast lo mid
      where
        mid = lo + (hi - lo) `div` 2

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
      (outcome, post) = outcomeOf (fmap fst res)
      outcomeOf result = case (decoded, result) of
        (Left e, _) -> (refused (show e) [failurePath (show e)], Nothing)
        (Right _, Left fs) -> (refused (show (NE.toList fs)) (map (failurePath . show) (NE.toList fs)), Nothing)
        (Right _, Right st') ->
          let p = st' ^. lsUTxOStateL . utxoL
           in ( A.object
                  [ "accepted"
                      A..= A.object
                        [ "consumed" A..= consumed pre p
                        , "produced" A..= pairs (produced pre p)
                        , "accounts_post" A..= accountsJson st'
                        ]
                  ]
              , Just st'
              )
      line =
        A.object $
          [ "n" A..= n
          , "slot" A..= unSlotNo (ledgerSlotNo env)
          , "epoch" A..= fmap unEpochNo epoch
          , "phase2" A..= if valid then A.String "valid" else A.object ["invalid" A..= submitted]
          , "tx" A..= hex bytes
          , "pparams" A..= A.toJSON (ledgerPp env)
          , "pre" A..= if prev == Just (continuity st) then A.String "follows_prev" else A.object ["fresh" A..= pairs pre]
          , "accounts_pre" A..= accountsJson st
          , "certs_pre" A..= certsJson st
          , "gov_pre" A..= govJson st
          , "treasury" A..= unCoin (casTreasury (ledgerAccount env))
          , "outcome" A..= outcome
          , "ex_units" A..= map evaluationJson (Map.toList topReport)
          , "sub_ex_units" A..= map (map evaluationJson . Map.toList) subReports
          , "min_fee" A..= either (const Nothing) (\t -> Just (unCoin (getMinFeeTxUtxo (ledgerPp env) t pre))) decoded
          , "time" A..= timeJson g (ledgerSlotNo env)
          ]
      (topReport, subReports) = levelReports (ledgerPp env) g pre stAnn
      -- The ledger's outcome for the same transaction under a true flag, the form a node receives it in.
      submitted =
        let flagged = tx & isPhase2ValidTxL .~ Phase2Valid
            annotated = mkStAnnTx (epochInfo g) (systemStart g) (ledgerPp env) pre mempty flagged
         in fst (outcomeOf (runReader (applySTS @(EraRule "LEDGER" E) (TRC (env, st, annotated))) g))
  keys <- either (\e -> fail ("record " <> show n <> ", " <> e)) pure (ruleAgrees (ledgerPp env) stAnn topReport subReports)
  modifyIORef' ruleKeys (+ keys)
  total <- readIORef ruleKeys
  when (keys > 0) $ hPutStrLn stderr ("rule arguments agree on " <> show total <> " redeemers")
  writeIORef lastPost (Just (continuity (fromMaybe st post)))
  BL8.appendFile out (A.encode line <> "\n")
  where
    continuity s = (s ^. lsUTxOStateL . utxoL, certsJson s, govJson s)
    certsJson s = case A.toJSON (s ^. lsCertStateL) of
      A.Object o -> A.Object (KM.delete "dstate" o)
      other -> other
    govJson s =
      let gov = s ^. lsUTxOStateL . utxosGovStateL
          GovRelation {grPParamUpdate, grHardFork, grCommittee, grConstitution} = gov ^. cgsProposalsL . pRootsL
       in A.object
            [ "proposals" A..= (gov ^. cgsProposalsL)
            , "roots"
                A..= A.object
                  [ "pparam_update" A..= prRoot grPParamUpdate
                  , "hard_fork" A..= prRoot grHardFork
                  , "committee" A..= prRoot grCommittee
                  , "constitution" A..= prRoot grConstitution
                  ]
            , "committee" A..= (gov ^. cgsCommitteeL)
            , "constitution" A..= (gov ^. cgsConstitutionL)
            ]
    evaluationJson (purpose, (result, args)) =
      A.object
        [ "purpose" A..= hex (serialize' (eraProtVerHigh @E) purpose)
        , "args" A..= fmap (map (hex . BL.toStrict . serialise)) args
        , "evaluation" A..= case result of
            Right (_, ExUnits mem steps) -> A.object ["ran" A..= [mem, steps]]
            Left failure -> A.object ["failed" A..= take 300 (show failure)]
        , "logs" A..= case result of
            Right (logs, _) -> logs
            Left (ValidationFailure _ _ logs _) -> logs
            Left _ -> []
        ]

-- | Each redeemer's evaluation at the maximum budget with the data arguments of the context built for it, none when none was built.
type Report = Map.Map (PlutusPurpose AsIx E) (Either (TransactionScriptFailure E) ([Text], ExUnits), Maybe [P.Data])

-- | The redeemer reports of a transaction, its top level given the results of its sub transactions, then each sub transaction in order.
levelReports :: PParams E -> Globals -> UTxO E -> DijkstraStAnnTx TopTx E -> (Report, [Report])
levelReports pp g utxo DijkstraStAnnTopTx {dsattTx, dsattSubTransactions} =
  (levelReport pp g utxo provided (LedgerTopTxInfo subResults) dsattTx, zipWith sub [0 ..] dsattSubTransactions)
  where
    provided = getScriptsProvided utxo dsattTx
    subResults = Map.fromList [(txIdTx dsastTx, dsastTxInfoResult) | DijkstraStAnnSubTx {dsastTx, dsastTxInfoResult} <- dsattSubTransactions]
    sub :: Int -> DijkstraStAnnTx SubTx E -> Report
    sub ix DijkstraStAnnSubTx {dsastTx} = levelReport pp g utxo provided (LedgerSubTxInfo (TxIx (fromIntegral ix))) dsastTx

-- | The number of redeemers whose context the rule built, or the first level where the rule's redeemers or their data arguments differ from the reports'.
ruleAgrees :: PParams E -> DijkstraStAnnTx TopTx E -> Report -> [Report] -> Either String Int
ruleAgrees pp DijkstraStAnnTopTx {dsattScriptsNeeded, dsattScriptsProvided, dsattPlutusScriptsWithContext, dsattSubTransactions} top subs = do
  topKeys <- level "top" dsattScriptsProvided dsattScriptsNeeded dsattPlutusScriptsWithContext top
  subKeys <- sequence (zipWith3 sub [0 ..] dsattSubTransactions subs)
  pure (topKeys + sum subKeys)
  where
    sub :: Int -> DijkstraStAnnTx SubTx E -> Report -> Either String Int
    sub ix DijkstraStAnnSubTx {dsastScriptsNeeded, dsastScriptsProvided, dsastPlutusScriptsWithContext} =
      level ("sub " <> show ix) dsastScriptsProvided dsastScriptsNeeded dsastPlutusScriptsWithContext
    level name provided needed collected report = case collected of
      Left _ -> Right 0
      Right pwcs
        | length used /= length pwcs -> Left (name <> ": " <> show (length used) <> " scripts used, " <> show (length pwcs) <> " contexts")
        | ruled /= dumped -> Left (name <> ": the arguments differ at " <> show differing)
        | otherwise -> Right (Map.size ruled)
        where
          (_, used) = resolveNeededPlutusScriptsWithPurpose (pp ^. ppProtocolVersionL) provided needed mempty
          ruled = Map.fromList (zip (map (hoistPlutusPurpose toAsIx . fst) (reverse used)) (map pwcData pwcs))
          dumped = Map.mapMaybe snd report
          differing = [k | k <- Map.keys (Map.union ruled dumped), Map.lookup k ruled /= Map.lookup k dumped]

-- | The redeemer report of one level of a transaction, in the context its level information gives.
levelReport :: PParams E -> Globals -> UTxO E -> ScriptsProvided E -> LedgerLevelTxInfo l E -> Tx l E -> Report
levelReport pp g utxo provided@(ScriptsProvided scripts) level tx = Map.mapWithKey run (tx ^. witsTxL . rdmrsTxWitsL . unRedeemersL)
  where
    protVer = pp ^. ppProtocolVersionL
    costModels = costModelsValid (pp ^. ppCostModelsL)
    needed@(AlonzoScriptsNeeded scriptsNeeded) = getScriptsNeeded utxo (tx ^. bodyTxL)
    byPointer = fromElems (\(purpose, _) -> hoistPlutusPurpose toAsIx purpose) scriptsNeeded
    (_, used) = resolveNeededPlutusScriptsWithPurpose protVer provided needed mempty
    lti =
      LedgerTxInfo
        { ltiProtVer = protVer
        , ltiEpochInfo = epochInfo g
        , ltiSystemStart = systemStart g
        , ltiUTxO = utxo
        , ltiTx = tx
        , ltiScriptsUsed = used
        , ltiScriptHashesUsed = toScriptHashByPurpose used
        , ltiLevelTxInfo = level
        }
    infoResult = mkTxInfoResult lti
    unresolved = Map.map (\(purpose, hash) -> (hoistPlutusPurpose toAsItem purpose, lookupPlutusScript hash scripts, hash)) byPointer
    run pointer (redeemerData, exUnits) = case built of
      Left failure -> (Left failure, Nothing)
      Right pwc -> (evaluated pwc, Just (map (const (P.I 0)) (pwcData pwc)))
      where
        built = do
          (purpose, hash) <- maybe (Left (RedeemerPointsToUnknownScriptHash pointer)) Right (Map.lookup pointer byPointer)
          script <- maybe (Left (MissingScript pointer unresolved)) Right (lookupPlutusScript hash scripts)
          let lang = plutusScriptLanguage script
          costModel <- maybe (Left (NoCostModelInLedgerState lang)) Right (Map.lookup lang costModels)
          first ContextError $
            mkPlutusWithContext (mkSupportedPlutusRunnable (pvMajor protVer) script) purpose lti infoResult redeemerData (pp ^. ppMaxTxExUnitsL) costModel
        evaluated pwc = case evaluatePlutusWithContext P.Verbose pwc of
          (logs, Left err) -> Left (ValidationFailure exUnits err logs pwc)
          (logs, Right budget) -> maybe (Left (IncompatibleBudget budget)) (Right . (,) logs) (exBudgetToExUnits budget)
