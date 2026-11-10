{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE NamedFieldPuns #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Writes the Dijkstra Imp spec corpus, one line per LEDGER rule run, from the rule modules the ledger's Dijkstra suite runs, or with `--pwc` one line per script the run applies.
module Main (main) where

import Cardano.Ledger.Alonzo.PParams (ppMaxTxExUnitsL)
import Cardano.Ledger.BaseTypes (Globals, natVersion)
import Cardano.Ledger.Core
import Cardano.Ledger.Dijkstra.Tx (DijkstraStAnnTx (..))
import Cardano.Ledger.Plutus.CostModels (getCostModelLanguage, getCostModelParams)
import Cardano.Ledger.Plutus.Evaluate (PlutusWithContext (..), evaluatePlutusWithContext)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language
import Cardano.Ledger.Shelley.Rules (LedgerEnv (..))
import Codec.Serialise (serialise)
import Control.Monad.IO.Class (liftIO)
import Control.State.Transition.Extended (STS (..), TRC (..))
import qualified Data.Aeson as A
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Lazy.Char8 as BL8
import qualified Data.ByteString.Short as SBS
import Data.IORef (readIORef, writeIORef)
import qualified Data.List.NonEmpty as NE
import Dump (E, counter, evalJson, hex)
import Lens.Micro ((^.))
import Mutate (Mutants, capturingKeys, recordTo, withMutants)
import qualified PlutusLedgerApi.Common as P
import qualified PlutusLedgerApi.V3 as PV3
import System.Environment (getArgs, withArgs)
import qualified Test.Cardano.Ledger.Allegra.Imp.UtxoSpec as AllegraUTXO
import qualified Test.Cardano.Ledger.Alonzo.Imp.BbodySpec as AlonzoBBODY
import qualified Test.Cardano.Ledger.Alonzo.Imp.UtxoSpec as AlonzoUTXO
import qualified Test.Cardano.Ledger.Alonzo.Imp.UtxosSpec as AlonzoUTXOS
import qualified Test.Cardano.Ledger.Alonzo.Imp.UtxowSpec as AlonzoUTXOW
import qualified Test.Cardano.Ledger.Babbage.Imp.UtxoSpec as BabbageUTXO
import qualified Test.Cardano.Ledger.Babbage.Imp.UtxosSpec as BabbageUTXOS
import qualified Test.Cardano.Ledger.Babbage.Imp.UtxowSpec as BabbageUTXOW
import qualified Test.Cardano.Ledger.Conway.Imp.BbodySpec as ConwayBBODY
import qualified Test.Cardano.Ledger.Conway.Imp.CertsSpec as ConwayCERTS
import qualified Test.Cardano.Ledger.Conway.Imp.DelegSpec as ConwayDELEG
import qualified Test.Cardano.Ledger.Conway.Imp.EnactSpec as ConwayENACT
import qualified Test.Cardano.Ledger.Conway.Imp.EpochSpec as ConwayEPOCH
import qualified Test.Cardano.Ledger.Conway.Imp.GovCertSpec as ConwayGOVCERT
import qualified Test.Cardano.Ledger.Conway.Imp.GovSpec as ConwayGOV
import qualified Test.Cardano.Ledger.Conway.Imp.HardForkSpec as ConwayHARDFORK
import qualified Test.Cardano.Ledger.Conway.Imp.LedgerSpec as ConwayLEDGER
import qualified Test.Cardano.Ledger.Conway.Imp.MempoolSpec as ConwayMEMPOOL
import qualified Test.Cardano.Ledger.Conway.Imp.RatifySpec as ConwayRATIFY
import qualified Test.Cardano.Ledger.Conway.Imp.SnapSpec as ConwaySNAP
import qualified Test.Cardano.Ledger.Conway.Imp.UtxoSpec as ConwayUTXO
import qualified Test.Cardano.Ledger.Conway.Imp.UtxosSpec as ConwayUTXOS
import qualified Test.Cardano.Ledger.Conway.Imp.UtxowSpec as ConwayUTXOW
import qualified Test.Cardano.Ledger.Dijkstra.Imp.BbodySpec as BBODY
import qualified Test.Cardano.Ledger.Dijkstra.Imp.CertSpec as CERT
import qualified Test.Cardano.Ledger.Dijkstra.Imp.EntitiesSpec as ENTITIES
import qualified Test.Cardano.Ledger.Dijkstra.Imp.LedgerSpec as LEDGER
import qualified Test.Cardano.Ledger.Dijkstra.Imp.MempoolSpec as MEMPOOL
import qualified Test.Cardano.Ledger.Dijkstra.Imp.PoolSpec as POOL
import qualified Test.Cardano.Ledger.Dijkstra.Imp.SnapSpec as SNAP
import qualified Test.Cardano.Ledger.Dijkstra.Imp.SubGovSpec as SUBGOV
import qualified Test.Cardano.Ledger.Dijkstra.Imp.SubLedgerSpec as SUBLEDGER
import qualified Test.Cardano.Ledger.Dijkstra.Imp.SubUtxoSpec as SUBUTXO
import qualified Test.Cardano.Ledger.Dijkstra.Imp.SubUtxowSpec as SUBUTXOW
import qualified Test.Cardano.Ledger.Dijkstra.Imp.UtxoSpec as UTXO
import qualified Test.Cardano.Ledger.Dijkstra.Imp.UtxosSpec as UTXOS
import qualified Test.Cardano.Ledger.Dijkstra.Imp.UtxowSpec as UTXOW
import Test.Cardano.Ledger.Dijkstra.ImpTest hiding (PlutusArgs)
import Test.Cardano.Ledger.Imp.Common
import qualified Test.Cardano.Ledger.Mary.Imp.UtxoSpec as MaryUTXO
import qualified Test.Cardano.Ledger.Shelley.Imp.BbodySpec as ShelleyBBODY
import qualified Test.Cardano.Ledger.Shelley.Imp.DelegSpec as ShelleyDELEG
import qualified Test.Cardano.Ledger.Shelley.Imp.EpochSpec as ShelleyEPOCH
import qualified Test.Cardano.Ledger.Shelley.Imp.LedgerSpec as ShelleyLEDGER
import qualified Test.Cardano.Ledger.Shelley.Imp.PoolSpec as ShelleyPOOL
import qualified Test.Cardano.Ledger.Shelley.Imp.UtxoSpec as ShelleyUTXO
import qualified Test.Cardano.Ledger.Shelley.Imp.UtxowSpec as ShelleyUTXOW

-- | The data arguments the script is applied to, in order.
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

-- | The line of one script with its context, evaluated with the units its redeemer declares and with the transaction maximum.
pwcJson :: Int -> String -> Int -> ExUnits -> PlutusWithContext -> A.Value
pwcJson n level k maxUnits pwc = case pwc of
  PlutusWithContext {pwcProtocolVersion, pwcScript, pwcArgs, pwcExUnits, pwcCostModel} ->
    let Plutus (PlutusBinary sbs) = plutusRunnableBinary pwcScript
        atMax = PlutusWithContext {pwcProtocolVersion, pwcScript, pwcArgs, pwcExUnits = maxUnits, pwcCostModel}
        ExUnits dm ds = pwcExUnits
     in A.object
          [ "n" A..= n
          , "level" A..= level
          , "k" A..= k
          , "language" A..= show (plutusLanguage pwcScript)
          , "protocol_version" A..= show pwcProtocolVersion
          , "script" A..= hex (SBS.fromShort sbs)
          , "args" A..= map (hex . BL.toStrict . serialise) (argsData pwcArgs)
          , "declared" A..= [dm, ds]
          , "cost_model_language" A..= show (getCostModelLanguage pwcCostModel)
          , "cost_model" A..= getCostModelParams pwcCostModel
          , "at_max" A..= evalJson (evaluatePlutusWithContext P.Verbose atMax)
          , "at_declared" A..= evalJson (evaluatePlutusWithContext P.Verbose pwc)
          ]

-- | Appends the scripts with context of the top transaction and of each sub transaction, as the ledger annotated them.
pwcTo ::
  FilePath ->
  Globals ->
  TRC (EraRule "LEDGER" E) ->
  Either (NE.NonEmpty (PredicateFailure (EraRule "LEDGER" E))) (State (EraRule "LEDGER" E), [Event (EraRule "LEDGER" E)]) ->
  ImpM t ()
pwcTo out _ (TRC (env, _, stAnn)) _ = liftIO $ do
  n <- readIORef counter
  writeIORef counter (n + 1)
  let maxUnits = ledgerPp env ^. ppMaxTxExUnitsL
      collected level = \case
        Left e -> [A.object ["n" A..= n, "level" A..= level, "collect_error" A..= take 300 (show (NE.toList e))]]
        Right pwcs -> zipWith (\k p -> pwcJson n level k maxUnits p) [0 ..] pwcs
      sub (i :: Int) s = case s of
        DijkstraStAnnSubTx {dsastPlutusScriptsWithContext} -> collected ("sub " <> show i) dsastPlutusScriptsWithContext
  lines' <- case stAnn of
    DijkstraStAnnTopTx {dsattPlutusScriptsWithContext, dsattSubTransactions} ->
      pure (collected "top" dsattPlutusScriptsWithContext ++ concat (zipWith sub [0 ..] dsattSubTransactions))
  mapM_ (\v -> BL8.appendFile out (A.encode v <> "\n")) lines'

-- | Writes the records, or with `--pwc` the scripts with their context.
main :: IO ()
main = withMutants "ic" 0 $ \mutants -> do
  args <- getArgs
  case args of
    "--pwc" : rest -> withArgs rest (corpus Scripts)
    _ -> corpus (Records mutants)

-- | What a corpus run writes for each LEDGER rule run.
data Writes = Records (Maybe Mutants) | Scripts

-- | Record numbers are positions in the corpus that the committed manifests name, so the inherited modules run after the Dijkstra ones.
corpus :: Writes -> IO ()
corpus writes =
  hspec $
    withImpInit @(LedgerSpec E) $
      capturingKeys $
        modifyImpInitProtVer @E (natVersion @12) $
          modifyImpInitPostSubmitTxHook
            ( \g trc res -> case writes of
                Records mutants -> recordTo mutants "/probe/out/ic.jsonl" g trc res
                Scripts -> pwcTo "/probe/out/pwc.jsonl" g trc res
            )
            $ do
          BBODY.spec
          LEDGER.spec
          MEMPOOL.spec
          SUBLEDGER.spec
          CERT.spec
          ENTITIES.spec
          SUBGOV.spec
          POOL.spec
          SNAP.spec
          UTXOW.spec
          UTXOS.spec
          UTXO.spec
          SUBUTXO.spec
          SUBUTXOW.spec
          POOL.dijkstraOnlySpec
          ShelleyBBODY.spec
          ShelleyDELEG.spec
          ShelleyEPOCH.spec
          ShelleyLEDGER.spec
          ShelleyPOOL.spec
          ShelleyUTXOW.spec
          ShelleyUTXO.spec
          AllegraUTXO.spec
          MaryUTXO.spec
          AlonzoBBODY.spec
          AlonzoUTXO.spec
          AlonzoUTXOS.spec
          AlonzoUTXOW.spec
          BabbageUTXO.spec
          BabbageUTXOW.spec
          BabbageUTXOS.spec
          ConwayBBODY.spec
          ConwayCERTS.spec
          ConwayDELEG.spec
          ConwayENACT.spec
          ConwayEPOCH.spec
          ConwayGOV.spec
          ConwayGOVCERT.spec
          ConwayLEDGER.spec
          ConwayMEMPOOL.spec
          ConwayHARDFORK.spec
          ConwayRATIFY.spec
          ConwaySNAP.spec
          ConwayUTXO.spec
          ConwayUTXOS.spec
          ConwayUTXOW.spec
