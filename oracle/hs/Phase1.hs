{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE NumericUnderscores #-}
{-# LANGUAGE OverloadedLists #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Writes the phase one corpus, the refused and accepted sides of phase one rules that the other corpora lack.
module Main (main) where

import Cardano.Ledger.Address (Addr (..))
import Cardano.Ledger.Alonzo.Scripts (AsIx (..), PlutusBinary (..), mkBinaryPlutusScript, pattern SpendingPurpose)
import Cardano.Ledger.Alonzo.TxWits (Redeemers (..), TxDats (..))
import Cardano.Ledger.BaseTypes (Network (..), StrictMaybe (..), mkCertIxPartial, mkTxIxPartial, natVersion)
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Credential (Credential (..), Ptr (..), SlotNo32 (..), StakeReference (..))
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Mary.Value (AssetName (..), MaryValue (..), PolicyID (..), multiAssetFromList)
import Cardano.Ledger.Metadata (Metadatum (..))
import Cardano.Ledger.Plutus.Data (Data (..), Datum (..), hashData)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (Language (..))
import Cardano.Ledger.Shelley.Scripts (pattern RequireSignature)
import Cardano.Ledger.Tools (setMinCoinTxOut)
import Cardano.Ledger.Val (inject)
import Codec.CBOR.Term (Term (..))
import Control.Monad (replicateM, void)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import Data.ByteString.Short (toShort)
import Data.Primitive.ByteArray (byteArrayFromList)
import qualified Data.Set as Set
import Data.Word (Word8)
import Dump (E)
import Mutate (capturingKeys, recordTo, withMutants)
import Edit
import Lens.Micro ((&), (.~))
import qualified PlutusLedgerApi.V1 as PV1
import Test.Cardano.Ledger.Core.Utils (txInAt)
import Test.Cardano.Ledger.Dijkstra.ImpTest
import Test.Cardano.Ledger.Imp.Common

phase1 :: SpecWith (ImpInit (LedgerSpec E))
phase1 = describe "PHASE1" $ do
  describe "refused" $ do
    it "a fee below the minimum" $
      void $ trySubmitTx (plain & bodyTxL . feeTxBodyL .~ Coin 1)
    it "an empty input set" $
      submitEdited (body (setKey 0 emptySet)) plain
    it "an output to the other network" $ do
      holder <- freshKeyHash
      void $ trySubmitTx (paying (mkBasicTxOut (Addr Mainnet (KeyHashObj holder) StakeRefNull) (inject (Coin 5_000_000))))
    it "an output over the maximum value size" $ do
      pp <- getsPParams id
      policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
      holder <- freshKeyAddrNoPtr_
      let token = multiAssetFromList [(policy, AssetName (toShort (BS.replicate 28 0 <> BS.pack [0, 0, hi, lo])), 1) | i <- [1 .. 160 :: Int], let (hi, lo) = (fromIntegral (i `div` 256), fromIntegral (i `mod` 256))]
      void $ trySubmitTx (plain & bodyTxL . mintTxBodyL .~ token & bodyTxL . outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut holder (MaryValue mempty token))])
    it "an output below the minimum coin" $ do
      holder <- freshKeyAddrNoPtr_
      void $ trySubmitTx (paying (mkBasicTxOut holder (inject (Coin 1))))
    it "an output to a bootstrap address with oversized attributes" $ do
      holder <- freshBootstrapAddressOversizedPayload
      void $ trySubmitTx (paying (mkBasicTxOut (AddrBootstrap holder) (inject (Coin 5_000_000))))
    it "redeemer execution units over the transaction limit" $
      spendV3 (submitEdited (wits (overKey 5 (overFirstValue (overIndex 1 (overIndex 0 (const (TInt 2_000_000_000)))))))) id
    it "a collateral input locked by a script" $ do
      locked <- sendCoinTo (Addr Testnet (ScriptHashObj alwaysHash) StakeRefNull) (Coin 30_000_000)
      spendV3 (void . trySubmitTx) (bodyTxL . collateralInputsTxBodyL .~ [locked])
    it "a collateral input holding a token" $ do
      pp <- getsPParams id
      policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
      holder <- freshKeyAddrNoPtr_
      let token = multiAssetFromList [(policy, AssetName "a", 1)]
          out = setMinCoinTxOut pp (mkBasicTxOut holder (MaryValue (Coin 30_000_000) token))
      minted <- submitTx (plain & bodyTxL . mintTxBodyL .~ token & bodyTxL . outputsTxBodyL .~ [out])
      passTick
      spendV3 (void . trySubmitTx) (bodyTxL . collateralInputsTxBodyL .~ [txInAt 0 minted])
    it "four collateral inputs" $ do
      collateral <- replicateM 4 makeCollateralInput
      spendV3 (void . trySubmitTx) (bodyTxL . collateralInputsTxBodyL .~ Set.fromList collateral)
    it "a script spend without collateral" $
      spendV3 (submitEdited (body (dropKey 13))) id
    it "a total collateral that the collateral does not pay" $
      spendV3 (void . trySubmitTx) (bodyTxL . totalCollateralTxBodyL .~ SJust (Coin 1))
    it "a collateral return to a pointer address" $ do
      holder <- freshKeyHash
      let pointer = Addr Testnet (KeyHashObj holder) (StakeRefPtr (Ptr (SlotNo32 1) (mkTxIxPartial 0) (mkCertIxPartial 0)))
      spendV3 (void . trySubmitTx) (bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut pointer (inject (Coin 5_000_000))))
    it "a script integrity hash that does not match at the top level" $
      spendV3 (submitEdited (body (setKey 11 (TBytes (BS.replicate 32 0))))) id
    it "a script integrity hash that does not match at a sub transaction" $
      subSubmitted (setKey 11 (TBytes (BS.replicate 32 0))) id
    it "a malformed script witness at the top level" $ do
      bad <- malformed
      void $ trySubmitTx (plain & witsTxL . scriptTxWitsL .~ [(hashScript bad, bad)])
    it "a malformed script witness at a sub transaction" $ do
      bad <- malformed
      subSubmitted id (witsTxL . scriptTxWitsL .~ [(hashScript bad, bad)])
    it "a malformed reference script at the top level" $ do
      bad <- malformed
      holder <- freshKeyAddrNoPtr_
      void $ trySubmitTx (paying (mkBasicTxOut holder (inject (Coin 20_000_000)) & referenceScriptTxOutL .~ SJust bad))
    it "a malformed reference script at a sub transaction" $ do
      bad <- malformed
      holder <- freshKeyAddrNoPtr_
      subSubmitted id (bodyTxL . outputsTxBodyL .~ [mkBasicTxOut holder (inject (Coin 20_000_000)) & referenceScriptTxOutL .~ SJust bad])
    it "a sub transaction over the maximum transaction size" $
      subSubmitted id (auxDataTxL .~ SJust (metadata [(1, List [B (byteArrayFromList (replicate 64 (fromIntegral i :: Word8))) | i <- [1 .. 300 :: Int]])]))
    it "a reference input given twice in its set" $ do
      x <- freshFundedTxIn
      submitEdited (body (overKey 18 dupFirst)) (plain & bodyTxL . referenceInputsTxBodyL .~ [x])
    it "a collateral input given twice in its set" $
      spendV3 (submitEdited (body (overKey 13 dupFirst))) id
  describe "accepted" $ do
    it "auxiliary data with its hash at the top level" $
      submitTx_ (plain & auxDataTxL .~ SJust (metadata [(1, I 1)]))
    it "auxiliary data with its hash at a sub transaction" $ do
      x <- freshFundedTxIn
      submitTx_ (mkTopTxWithSubTxs [mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [x]) & auxDataTxL .~ SJust (metadata [(1, I 1)]) :: Tx SubTx E])
    it "a network id in the body" $
      submitTx_ (plain & bodyTxL . networkIdTxBodyL .~ SJust Testnet)
    it "a collateral return" $ do
      holder <- freshKeyAddrNoPtr_
      spendV3 submitTx_ (bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut holder (inject (Coin 5_000_000))))
    it "a total collateral that the collateral pays" $ do
      holder <- freshKeyAddrNoPtr_
      collateral <- makeCollateralInput
      spendV3 submitTx_ $ \tx ->
        tx
          & bodyTxL . collateralInputsTxBodyL .~ [collateral]
          & bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut holder (inject (Coin 5_000_000)))
          & bodyTxL . totalCollateralTxBodyL .~ SJust (Coin 25_000_000)
    it "a mint under a native script" $ do
      pp <- getsPParams id
      policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
      holder <- freshKeyAddrNoPtr_
      let token = multiAssetFromList [(policy, AssetName "a", 1)]
      submitTx_ (plain & bodyTxL . mintTxBodyL .~ token & bodyTxL . outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut holder (MaryValue mempty token))])
  describe "state" $ do
    it "an output with a datum hash and its datum" $ do
      holder <- freshKeyAddrNoPtr_
      let datum = Data (PV1.I 42) :: Data E
      submitTx_ (paying (mkBasicTxOut holder (inject (Coin 5_000_000)) & datumTxOutL .~ DatumHash (hashData datum)) & witsTxL . datsTxWitsL .~ TxDats [(hashData datum, datum)])
    it "a stake credential registration" $
      void . registerStakeCredential . KeyHashObj =<< freshKeyHash
  where
    plain :: Tx TopTx E
    plain = mkBasicTx mkBasicTxBody
    paying out = plain & bodyTxL . outputsTxBodyL .~ [out]
    body f = noEdit {editBody = f}
    wits f = noEdit {editWits = f}
    metadata entries = mkBasicTxAuxData & metadataTxAuxDataL .~ entries

-- | A body map without the entry at the key.
dropKey :: Int -> Term -> Term
dropKey k (TMap kvs) = TMap [(key, v) | (key, v) <- kvs, key /= TInt k]
dropKey _ t = t

-- | A V3 script whose bytes do not deserialise.
malformed :: ImpTestM E (Script E)
malformed = fromPlutusScript <$> expectJust (mkBinaryPlutusScript PlutusV3 (PlutusBinary (toShort "\x01\x02\x03")))

-- | The V3 script that returns unit, as its CBOR hex.
alwaysHex :: ByteString
alwaysHex = "4901010023357369324d"

-- | The script that returns unit.
always :: Script E
always = maybe (error "the script does not decode") fromPlutusScript (mkBinaryPlutusScript PlutusV3 . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode alwaysHex))

-- | The hash of the script that returns unit.
alwaysHash :: ScriptHash
alwaysHash = hashScript always

-- | Locks coin at the script that returns unit, then submits a spend of it after the change to the transaction.
spendV3 :: (Tx TopTx E -> ImpTestM E ()) -> (Tx TopTx E -> Tx TopTx E) -> ImpTestM E ()
spendV3 submit change = do
  locked <- sendCoinTo (Addr Testnet (ScriptHashObj alwaysHash) StakeRefNull) (Coin 5_000_000)
  collateral <- makeCollateralInput
  let tx =
        mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [locked] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 400_000)
          & witsTxL . scriptTxWitsL .~ [(alwaysHash, always)]
          & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (Data (PV1.I 0), ExUnits 1_000_000 1_000_000_000))]
  submit (change tx)

-- | Submits a top level transaction with one sub transaction that spends a fresh input, after the change to the sub transaction and the edit to its body.
subSubmitted :: (Term -> Term) -> (Tx SubTx E -> Tx SubTx E) -> ImpTestM E ()
subSubmitted f change = do
  x <- freshFundedTxIn
  let sub = change (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [x]))
  submitEdited noEdit {editSubBody = firstSub f} (mkTopTxWithSubTxs [sub])

main :: IO ()
main =
  withMutants "p1" 2000000 $ \mutants ->
    hspec $
      withImpInit @(LedgerSpec E) $
        capturingKeys $
          modifyImpInitProtVer @E (natVersion @12) $
            modifyImpInitPostSubmitTxHook (recordTo mutants "/probe/out/p1.jsonl") phase1
