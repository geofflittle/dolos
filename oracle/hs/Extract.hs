{-# LANGUAGE DataKinds #-}
{-# LANGUAGE TypeApplications #-}

-- | Writes the Dijkstra Imp spec corpus, one line per LEDGER rule run, from the rule modules the ledger's Dijkstra suite runs.
module Main (main) where

import Cardano.Ledger.BaseTypes (natVersion)
import Dump (E, dumpTo)
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
import Test.Cardano.Ledger.Dijkstra.ImpTest
import Test.Cardano.Ledger.Imp.Common
import qualified Test.Cardano.Ledger.Mary.Imp.UtxoSpec as MaryUTXO
import qualified Test.Cardano.Ledger.Shelley.Imp.BbodySpec as ShelleyBBODY
import qualified Test.Cardano.Ledger.Shelley.Imp.DelegSpec as ShelleyDELEG
import qualified Test.Cardano.Ledger.Shelley.Imp.EpochSpec as ShelleyEPOCH
import qualified Test.Cardano.Ledger.Shelley.Imp.LedgerSpec as ShelleyLEDGER
import qualified Test.Cardano.Ledger.Shelley.Imp.PoolSpec as ShelleyPOOL
import qualified Test.Cardano.Ledger.Shelley.Imp.UtxoSpec as ShelleyUTXO
import qualified Test.Cardano.Ledger.Shelley.Imp.UtxowSpec as ShelleyUTXOW

-- | Record numbers are positions in the corpus that the committed manifests name, so the inherited modules run after the Dijkstra ones.
main :: IO ()
main =
  hspec $
    withImpInit @(LedgerSpec E) $
      modifyImpInitProtVer @E (natVersion @12) $
        modifyImpInitPostSubmitTxHook (dumpTo "/probe/out/ic.jsonl") $ do
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
