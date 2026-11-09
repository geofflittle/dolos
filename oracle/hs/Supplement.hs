{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE NumericUnderscores #-}
{-# LANGUAGE OverloadedLists #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Writes the supplement corpus, scenarios whose shapes the Imp spec corpus lacks.
module Main (main) where

import qualified Cardano.Chain.Common as Byron
import Cardano.Crypto.Hash (hashToBytes)
import Cardano.Crypto.Hashing (unsafeAbstractHashFromBytes)
import Cardano.Ledger.Address (AccountAddress (..), AccountId (..), Addr (..), BootstrapAddress (..), Withdrawals (..), bootstrapKeyHash)
import Cardano.Ledger.Alonzo.Scripts (AsIx (..), PlutusBinary (..), mkBinaryPlutusScript, pattern CertifyingPurpose, pattern MintingPurpose, pattern RewardingPurpose, pattern SpendingPurpose)
import Cardano.Ledger.Alonzo.TxAuxData (AlonzoEraTxAuxData (..))
import Cardano.Ledger.Alonzo.TxWits (Redeemers (..), TxDats (..))
import Cardano.Ledger.BaseTypes (Globals (..), Network (..), StrictMaybe (..), epochInfoPure, mkCertIxPartial, mkTxIxPartial, natVersion, strictMaybeToMaybe)
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Conway.Governance (Constitution (..), ConwayEraGov (..), GovAction (..), Vote (..), Voter (..), VotingProcedure (..), VotingProcedures (..), constitutionGuardrailsScriptHashL)
import Cardano.Ledger.Conway.Scripts (pattern ProposingPurpose, pattern VotingPurpose)
import Cardano.Ledger.Conway.TxCert (Delegatee (..), pattern DelegTxCert, pattern RegDepositDelegTxCert, pattern RegDepositTxCert, pattern UpdateDRepTxCert)
import Cardano.Ledger.Credential (Credential (..), Ptr (..), SlotNo32 (..), StakeReference (..))
import Cardano.Ledger.DRep (DRep (..))
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Dijkstra.PParams
import Cardano.Ledger.Dijkstra.Scripts (AccountBalanceInterval (..), AccountBalanceIntervals (..), pattern GuardingPurpose, pattern RequireGuard)
import Cardano.Ledger.Hashes (EraIndependentTxBody, SafeHash, ScriptHash, extractHash, hashAnnotated)
import Cardano.Ledger.Keys (KeyHash (..), KeyRole (..), coerceKeyRole)
import Cardano.Ledger.Keys.Bootstrap (BootstrapWitness (..), ChainCode (..), bootstrapWitKeyHash, makeBootstrapWitness)
import Cardano.Ledger.Mary.Value (AssetName (..), MaryValue (..), PolicyID (..), multiAssetFromList)
import Cardano.Ledger.Metadata (Metadatum (..))
import Cardano.Ledger.Plutus.CostModels (mkCostModelsLenient)
import Cardano.Ledger.Plutus.Data (Data (..), Datum (..), dataToBinaryData, hashData)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (Language (..), SLanguage (..), hashPlutusScript)
import Cardano.Ledger.Plutus.TxInfo (transAddr, transCoinToLovelace, transCred, transKeyHash, transScriptHash)
import Cardano.Ledger.Shelley.LedgerState (newEpochStateGovStateL)
import Cardano.Ledger.Shelley.Scripts (pattern RequireAllOf, pattern RequireSignature)
import Cardano.Ledger.State (PoolMetadata (..), sppMetadataL)
import Cardano.Ledger.Tools (setMinCoinTxOut)
import Cardano.Ledger.TxIn (TxIn, mkTxInPartial)
import Cardano.Ledger.Val (inject)
import Cardano.Slotting.EpochInfo (EpochInfo (..), epochInfoEpoch, epochInfoSize, epochInfoSlotLength)
import Cardano.Slotting.Slot (EpochSize (..), SlotNo (..))
import Cardano.Slotting.Time (SystemStart (..), slotLengthToMillisec)
import Codec.CBOR.Term (Term (..))
import Control.Exception (SomeException)
import Cardano.Ledger.Alonzo.PParams (ppCostModelsL)
import Control.Monad (forM_, replicateM_, void, when, (<=<))
import Control.Monad.IO.Class (liftIO)
import qualified Data.Aeson as A
import Data.Aeson.Types (parseEither)
import Data.Int (Int64)
import qualified Data.Map.Strict as Map
import Data.Text (Text)
import qualified PlutusCore.Crypto.BLS12_381.G1 as G1
import qualified PlutusCore.Crypto.BLS12_381.G2 as G2
import Data.Foldable (toList)
import Data.List (isInfixOf)
import Data.Functor.Identity (runIdentity)
import Data.List.NonEmpty (NonEmpty (..))
import Data.Time.Clock.POSIX (utcTimeToPOSIXSeconds)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Lazy as BL
import Data.ByteString.Short (toShort)
import Data.Maybe (listToMaybe)
import qualified Data.OMap.Strict as OMap
import Data.Primitive.ByteArray (byteArrayFromList)
import Data.Typeable (Typeable)
import Data.Word (Word64, Word8)
import Dump (E)
import Edit
import Mutate (capturingKeys, recordTo, withMutants)
import Lens.Micro ((%~), (&), (.~), (^.))
import Lens.Micro.Mtl (use, (%=))
import qualified PlutusCore as PLC
import qualified PlutusLedgerApi.V1 as PV1
import qualified PlutusLedgerApi.V3 as PV3
import Test.Cardano.Ledger.Core.KeyPair (ByronKeyPair (..))
import Test.Cardano.Ledger.Core.Utils (txInAt)
import Test.Cardano.Ledger.Dijkstra.ImpTest
import Test.Cardano.Ledger.Imp.Common
import Test.Cardano.Ledger.Plutus.Examples (alwaysSucceedsNoDatum)
import qualified UplcTerm as U

supplement :: SpecWith (ImpInit (LedgerSpec E))
supplement = describe "SUPPLEMENT" $ do
  it "a tx spends a child output of the batch before it in one slot" $ do
    pp <- getsPParams id
    spent <- freshFundedTxIn
    addr <- freshKeyAddr_
    let sub :: Tx SubTx E
        sub = mkBasicTx $ mkBasicTxBody & inputsTxBodyL .~ [spent] & outputsTxBodyL .~ [setMinCoinTxOut pp $ mkBasicTxOut addr mempty]
        y = mkTxInPartial (txIdTx sub) 0
    submitTx_ (mkTopTxWithSubTxs [sub])
    submitTx_ (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [y]) :: Tx TopTx E)
  it "a tx spends an input a child of the batch before it spent in one slot" $ do
    x <- freshFundedTxIn
    submitTx_ (mkTopTxWithSubTxs [mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [x]) :: Tx SubTx E])
    void $ trySubmitTx (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [x]) :: Tx TopTx E)
  it "the collateral return of a phase two invalid tx is spent in the next slot" $ do
    addr <- freshKeyAddrNoPtr_
    coll <- sendCoinTo addr (Coin 10_000_000)
    tx <- switchTxToPhase2InvalidLegacyMode (mkBasicTx mkBasicTxBody :: Tx TopTx E)
    let tx' = tx & bodyTxL . collateralInputsTxBodyL .~ [coll] & bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut addr (inject (Coin 5_000_000)))
    fixed <- submitPhase2Invalid tx'
    let n = length (fixed ^. bodyTxL . outputsTxBodyL)
        crIn = mkTxInPartial (txIdTx fixed) (fromIntegral n)
    passTick
    submitTx_ (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [crIn]) :: Tx TopTx E)
  it "a child spends a token output" $ do
    pp <- getsPParams id
    policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
    name <- arbitrary @AssetName
    holder <- freshKeyAddrNoPtr_
    let token = multiAssetFromList [(policy, name, 1)]
    minted <- submitTx (mkBasicTx (mkBasicTxBody & mintTxBodyL .~ token & outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut holder (MaryValue mempty token))]) :: Tx TopTx E)
    passTick
    funded <- freshFundedTxIn
    receiver <- freshKeyAddrNoPtr_
    let sub :: Tx SubTx E
        sub = mkBasicTx $ mkBasicTxBody & inputsTxBodyL .~ [txInAt 0 minted, funded] & outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut receiver (MaryValue mempty token))]
    submitTx_ (mkTopTxWithSubTxs [sub])
  it "a child output holds a hash datum given in the child witnesses, and a later child spends it" $ do
    pp <- getsPParams id
    holder <- freshKeyAddrNoPtr_
    funded <- freshFundedTxIn
    let datum = Data (PV1.I 42) :: Data E
        maker :: Tx SubTx E
        maker =
          mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [funded] & outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut holder mempty & datumTxOutL .~ DatumHash (hashData datum))])
            & witsTxL . datsTxWitsL .~ TxDats [(hashData datum, datum)]
    top <- submitTx (mkTopTxWithSubTxs [maker])
    made <- expectJust . listToMaybe . OMap.elems $ top ^. bodyTxL . subTransactionsTxBodyL
    passTick
    funded' <- freshFundedTxIn
    receiver <- freshKeyAddrNoPtr_
    let spender :: Tx SubTx E
        spender = mkBasicTx $ mkBasicTxBody & inputsTxBodyL .~ [txInAt 0 made, funded'] & outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut receiver mempty)]
    submitTx_ (mkTopTxWithSubTxs [spender])
  it "a child output carries a reference script" $ do
    pp <- getsPParams id
    holder <- freshKeyAddrNoPtr_
    funded <- freshFundedTxIn
    script <- fromNativeScript . RequireSignature <$> freshKeyHash
    let sub :: Tx SubTx E
        sub = mkBasicTx $ mkBasicTxBody & inputsTxBodyL .~ [funded] & outputsTxBodyL .~ [setMinCoinTxOut pp (mkBasicTxOut holder mempty & referenceScriptTxOutL .~ SJust script)]
    submitTx_ (mkTopTxWithSubTxs [sub])
  it "a tx names one input as both a spent and a reference input" $ do
    x <- freshFundedTxIn
    void $ trySubmitTx (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [x] & referenceInputsTxBodyL .~ [x]) :: Tx TopTx E)
  it "a partial withdrawal, then a legacy drain of the remainder in one slot" $ do
    account <- registerStakeCredential . KeyHashObj =<< freshKeyHash
    submitTx_ (mkBasicTx (mkBasicTxBody & directDepositsTxBodyL .~ DirectDeposits [(account, Coin 1_000_000)]) :: Tx TopTx E)
    passTick
    submitTx_ (mkBasicTx (mkBasicTxBody & withdrawalsTxBodyL .~ Withdrawals [(account, Coin 400_000)]) :: Tx TopTx E)
    drain <- switchTxToLegacyMode (mkBasicTx (mkBasicTxBody & withdrawalsTxBodyL .~ Withdrawals [(account, Coin 600_000)]))
    void $ trySubmitTx drain
  it "a registration, then a withdrawal from that account in one slot" $ do
    account <- registerStakeCredential . KeyHashObj =<< freshKeyHash
    submitTx_ (mkBasicTx (mkBasicTxBody & withdrawalsTxBodyL .~ Withdrawals [(account, Coin 0)]) :: Tx TopTx E)
  -- (program 1.1.0 (lam ctx (force [[[(force (builtin ifThenElse)) [[(builtin equalsInteger) [[(builtin indexByteString) (con bytestring #0a0b)] (con integer 1)]] (con integer 11)]] (delay (con unit ()))] (delay (error))])))
  it "a V3 script calls indexByteString" $
    spendWithV3 "581a01010025333573466e1ccdc72441020a0b004800920161498581"
  -- (program 1.1.0 (lam ctx (force [[[(force (builtin ifThenElse)) [[(builtin equalsByteString) [[[(builtin sliceByteString) (con integer 1)] (con integer 2)] (con bytestring #0a0b0c0d)]] (con bytestring #0b0c)]] (delay (con unit ()))] (delay (error))])))
  it "a V3 script calls sliceByteString" $
    spendWithV3 "582201010025333573466e3cccdc62400490022441040a0b0c0d004881020b0c00149859"
  it "the collateral return of a phase two invalid tx is spent in the same slot" $ do
    addr <- freshKeyAddrNoPtr_
    coll <- sendCoinTo addr (Coin 10_000_000)
    tx <- switchTxToPhase2InvalidLegacyMode (mkBasicTx mkBasicTxBody :: Tx TopTx E)
    let tx' = tx & bodyTxL . collateralInputsTxBodyL .~ [coll] & bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut addr (inject (Coin 5_000_000)))
    fixed <- submitPhase2Invalid tx'
    let n = length (fixed ^. bodyTxL . outputsTxBodyL)
        crIn = mkTxInPartial (txIdTx fixed) (fromIntegral n)
    submitTx_ (mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [crIn]) :: Tx TopTx E)
  -- (program 1.1.0 (lam ctx [[(force (builtin chooseUnit)) (con unit ())] (con unit ())]))
  it "a V3 script calls chooseUnit" $
    spendWithV3 "4901010023357369324d"
  -- (program 1.1.0 (lam ctx [[(force (builtin trace)) (con string "seen")] (con unit ())]))
  it "a V3 script calls trace" $
    spendWithV3 "500101002335738921047365656e004981"
  -- (program 1.1.0 (lam ctx (force [[[(force (builtin ifThenElse)) [(force (builtin nullList)) (con (list integer) [])]] (delay (con unit ()))] (delay (error))])))
  it "a V3 script calls nullList" $
    spendWithV3 "500101002533357346ae8d2f5802930b01"
  -- (program 1.1.0 (lam ctx (force [[[(force (builtin ifThenElse)) [[(builtin equalsInteger) [[(builtin quotientInteger) (con integer -7)] (con integer 2)]] (con integer -3)]] (delay (con unit ()))] (delay (error))])))
  it "a V3 script calls quotientInteger" $
    spendWithV3 "5601010025333573466e1ccdc22401a90022400a2930b1"
  -- (program 1.1.0 (lam ctx (force [[[(force (builtin ifThenElse)) [[(builtin equalsInteger) [(force (builtin headList)) [[(force (builtin dropList)) (con integer 2)] (con (list integer) [1, 2, 3])]]] (con integer 3)]] (delay (con unit ()))] (delay (error))])))
  it "a V3 script calls dropList" $
    spendWithV3 "581d01010025333573466e1cd5d099abd8480112f582050483120061498581"
  forM_ ([Top, Sub] :: [Level]) $ \level -> do
    it ("a bootstrap input is spent at the " <> show level) $ do
      bootAddr <- freshBootstrapAddress
      bootIn <- sendCoinTo (AddrBootstrap bootAddr) mempty
      submitTx_ (atLevel level (mkBasicTxBody & inputsTxBodyL .~ [bootIn]))
    it ("a bootstrap input is spent without its witness at the " <> show level) $ do
      bootAddr <- freshBootstrapAddress
      bootIn <- sendCoinTo (AddrBootstrap bootAddr) mempty
      void . expectLeft =<< withoutBootstrapWitnesses level (trySubmitTx (atLevel level (mkBasicTxBody & inputsTxBodyL .~ [bootIn])))
    it ("a bootstrap witness whose attributes hold a key Byron addresses do not define at the " <> show level) $
      customBootstrapSpend level (replicate 32 7) [0xa1, 0x03, 0x41, 0x00]
    it ("a bootstrap witness with a 31 byte chain code at the " <> show level) $
      customBootstrapSpend level (replicate 31 7) [0xa0]
    it ("a guard key only a bootstrap witness provides at the " <> show level) $ do
      bootAddr <- freshBootstrapAddress
      bootIn <- sendCoinTo (AddrBootstrap bootAddr) mempty
      let guardKey = KeyHashObj (coerceKeyRole (bootstrapKeyHash bootAddr))
      submitTx_ (atLevel level (mkBasicTxBody & inputsTxBodyL .~ [bootIn] & guardsTxBodyL .~ [guardKey]))
    it ("a native script signature only a bootstrap witness provides at the " <> show level) $ do
      bootAddr <- freshBootstrapAddress
      bootIn <- sendCoinTo (AddrBootstrap bootAddr) mempty
      script <- impAddNativeScript (RequireSignature (coerceKeyRole (bootstrapKeyHash bootAddr)))
      locked <- sendCoinTo (Addr Testnet (ScriptHashObj script) StakeRefNull) (Coin 5_000_000)
      void $ trySubmitTx (atLevel level (mkBasicTxBody & inputsTxBodyL .~ [bootIn, locked]))
  decoderRules
  -- (program 1.2.0 (lam ctx (con unit ())))
  it "a V3 script at UPLC 1.2.0" $
    spendWithV3 "450102002499"
  validityBounds
  contextValues
  readBackShapes
  it "a V3 script calls sha2_256" $
    spendWithV3 "4b010100232498dc92450001"
  it "a V3 script calls mkNilData" $
    spendWithV3 (U.scriptHex (U.lam (U.check (U.forced PLC.NullList [U.builtin PLC.MkNilData [U.unit]]))))
  it "a V3 script compares an integer above the 262144 bit bound" $
    spendWithV3In (void . trySubmitTx) (ValidityInterval SNothing SNothing) (Data (PV1.I 0)) (ExUnits 30_000 3_000_000_000) $
      U.scriptHex (U.lam (U.check (U.builtin PLC.LessThanInteger [U.int 1, squared 18 (U.int 2)])))
  tableCells
  builtinCalls

-- | Spends at a script of each language that calls one builtin, under the cost models the chain registered, at a V3 script that calls it on an argument of the wrong type, and at a V4 script that calls it under the default cost models.
builtinCalls :: SpecWith (ImpInit (LedgerSpec E))
builtinCalls = describe "builtin calls" $ do
  forM_ calls $ \(Call fun n args) -> do
    forM_ ([PlutusV1, PlutusV2, PlutusV3] :: [Language]) $ \language ->
      it ("a " <> show language <> " script calls " <> show fun) $
        callSpend submitTx_ language (U.forcedTimes n fun args)
    it ("a PlutusV4 script calls " <> show fun) $
      callSpend acceptedOrRefusedByScript PlutusV4 (U.forcedTimes n fun args)
    it ("a PlutusV3 script calls " <> show fun <> " on a first argument of the wrong type") $
      callSpend refusedByScript PlutusV3 (U.forcedTimes n fun (wrongFirst args))
  forM_ calls $ \(Call fun n args) ->
    it ("a PlutusV4 script calls " <> show fun <> " under the default cost models") $
      spendCalling submitTx_ PlutusV4 (U.forcedTimes n fun args)

-- | A builtin, the number of times it is forced, and arguments it returns on.
data Call = Call PLC.DefaultFun Int [U.T]

-- | The arguments with the first one replaced by a constant of another type.
wrongFirst :: [U.T] -> [U.T]
wrongFirst (first : rest) = (if first == U.unit then U.int 0 else U.unit) : rest
wrongFirst [] = []

-- | A call of each builtin that protocol 12 allows.
calls :: [Call]
calls =
  [ Call PLC.AddInteger 0 [U.int 1, U.int 2]
  , Call PLC.SubtractInteger 0 [U.int 1, U.int 2]
  , Call PLC.MultiplyInteger 0 [U.int 1, U.int 2]
  , Call PLC.DivideInteger 0 [U.int 7, U.int 2]
  , Call PLC.QuotientInteger 0 [U.int 7, U.int 2]
  , Call PLC.RemainderInteger 0 [U.int 7, U.int 2]
  , Call PLC.ModInteger 0 [U.int 7, U.int 2]
  , Call PLC.EqualsInteger 0 [U.int 1, U.int 2]
  , Call PLC.LessThanInteger 0 [U.int 1, U.int 2]
  , Call PLC.LessThanEqualsInteger 0 [U.int 1, U.int 2]
  , Call PLC.AppendByteString 0 [U.bytes "\1", U.bytes "\2"]
  , Call PLC.ConsByteString 0 [U.int 1, U.bytes ""]
  , Call PLC.SliceByteString 0 [U.int 0, U.int 1, U.bytes "\1\2"]
  , Call PLC.LengthOfByteString 0 [U.bytes "\1"]
  , Call PLC.IndexByteString 0 [U.bytes "\1", U.int 0]
  , Call PLC.EqualsByteString 0 [U.bytes "\1", U.bytes "\2"]
  , Call PLC.LessThanByteString 0 [U.bytes "\1", U.bytes "\2"]
  , Call PLC.LessThanEqualsByteString 0 [U.bytes "\1", U.bytes "\2"]
  , Call PLC.Sha2_256 0 [U.bytes ""]
  , Call PLC.Sha3_256 0 [U.bytes ""]
  , Call PLC.Blake2b_256 0 [U.bytes ""]
  , Call PLC.VerifyEd25519Signature 0 [hexBytes "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", U.bytes "", hexBytes "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"]
  , Call PLC.AppendString 0 [text "a", text "b"]
  , Call PLC.EqualsString 0 [text "a", text "b"]
  , Call PLC.EncodeUtf8 0 [text "a"]
  , Call PLC.DecodeUtf8 0 [U.bytes "a"]
  , Call PLC.IfThenElse 1 [U.con True, U.unit, U.unit]
  , Call PLC.ChooseUnit 1 [U.unit, U.unit]
  , Call PLC.Trace 1 [text "a", U.unit]
  , Call PLC.FstPair 2 [U.con (PV1.I 1, PV1.I 2)]
  , Call PLC.SndPair 2 [U.con (PV1.I 1, PV1.I 2)]
  , Call PLC.ChooseList 2 [integers [1], U.unit, U.unit]
  , Call PLC.MkCons 1 [U.int 1, integers []]
  , Call PLC.HeadList 1 [integers [1]]
  , Call PLC.TailList 1 [integers [1]]
  , Call PLC.NullList 1 [integers []]
  , Call PLC.ChooseData 1 (U.con (PV1.I 1) : replicate 5 U.unit)
  , Call PLC.ConstrData 0 [U.int 0, U.con ([] :: [PV1.Data])]
  , Call PLC.MapData 0 [U.con ([] :: [(PV1.Data, PV1.Data)])]
  , Call PLC.ListData 0 [U.con ([] :: [PV1.Data])]
  , Call PLC.IData 0 [U.int 1]
  , Call PLC.BData 0 [U.bytes ""]
  , Call PLC.UnConstrData 0 [U.con (PV1.Constr 0 [])]
  , Call PLC.UnMapData 0 [U.con (PV1.Map [])]
  , Call PLC.UnListData 0 [U.con (PV1.List [])]
  , Call PLC.UnIData 0 [U.con (PV1.I 1)]
  , Call PLC.UnBData 0 [U.con (PV1.B "")]
  , Call PLC.EqualsData 0 [U.con (PV1.I 1), U.con (PV1.I 2)]
  , Call PLC.MkPairData 0 [U.con (PV1.I 1), U.con (PV1.I 2)]
  , Call PLC.MkNilData 0 [U.unit]
  , Call PLC.MkNilPairData 0 [U.unit]
  , Call PLC.SerialiseData 0 [U.con (PV1.I 1)]
  , Call PLC.VerifyEcdsaSecp256k1Signature 0 [hexBytes "032e433589dce61863199171f4d1e3fa946a5832621fcd29559940a0950f96fb6f", hexBytes "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", hexBytes "4941155e2303988a1be97a021fbaf9fe6064d05ea694bc5e89328f297154e5c63a2f3e7b5f509294a4c2e22feb697a16b792fabfebe9d0f38403b1c929836b5a"]
  , Call PLC.VerifySchnorrSecp256k1Signature 0 [hexBytes "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9", U.bytes (BS.replicate 32 0), hexBytes "e907831f80848d1069a5371b402410364bdf1c5f8307b0084c55f1ce2dca821525f66a4a85ea8b71e482a74f382d2ce5ebeee8fdb2172f477df4900d310536c0"]
  , Call PLC.Bls12_381_G1_add 0 [g1, g1]
  , Call PLC.Bls12_381_G1_neg 0 [g1]
  , Call PLC.Bls12_381_G1_scalarMul 0 [U.int 2, g1]
  , Call PLC.Bls12_381_G1_equal 0 [g1, g1]
  , Call PLC.Bls12_381_G1_compress 0 [g1]
  , Call PLC.Bls12_381_G1_uncompress 0 [g1Bytes]
  , Call PLC.Bls12_381_G1_hashToGroup 0 [U.bytes "a", U.bytes "b"]
  , Call PLC.Bls12_381_G2_add 0 [g2, g2]
  , Call PLC.Bls12_381_G2_neg 0 [g2]
  , Call PLC.Bls12_381_G2_scalarMul 0 [U.int 2, g2]
  , Call PLC.Bls12_381_G2_equal 0 [g2, g2]
  , Call PLC.Bls12_381_G2_compress 0 [g2]
  , Call PLC.Bls12_381_G2_uncompress 0 [g2Bytes]
  , Call PLC.Bls12_381_G2_hashToGroup 0 [U.bytes "a", U.bytes "b"]
  , Call PLC.Bls12_381_millerLoop 0 [g1, g2]
  , Call PLC.Bls12_381_mulMlResult 0 [pairing, pairing]
  , Call PLC.Bls12_381_finalVerify 0 [pairing, pairing]
  , Call PLC.Keccak_256 0 [U.bytes ""]
  , Call PLC.Blake2b_224 0 [U.bytes ""]
  , Call PLC.IntegerToByteString 0 [U.con True, U.int 0, U.int 1]
  , Call PLC.ByteStringToInteger 0 [U.con True, U.bytes "\1"]
  , Call PLC.AndByteString 0 [U.con False, U.bytes "\1", U.bytes "\3"]
  , Call PLC.OrByteString 0 [U.con False, U.bytes "\1", U.bytes "\3"]
  , Call PLC.XorByteString 0 [U.con False, U.bytes "\1", U.bytes "\3"]
  , Call PLC.ComplementByteString 0 [U.bytes "\1"]
  , Call PLC.ReadBit 0 [U.bytes "\1", U.int 0]
  , Call PLC.WriteBits 0 [U.bytes "\1", integers [0], U.con True]
  , Call PLC.ReplicateByte 0 [U.int 1, U.int 0]
  , Call PLC.ShiftByteString 0 [U.bytes "\1", U.int 1]
  , Call PLC.RotateByteString 0 [U.bytes "\1", U.int 1]
  , Call PLC.CountSetBits 0 [U.bytes "\1"]
  , Call PLC.FindFirstSetBit 0 [U.bytes "\1"]
  , Call PLC.Ripemd_160 0 [U.bytes ""]
  , Call PLC.ExpModInteger 0 [U.int 2, U.int 3, U.int 5]
  , Call PLC.DropList 1 [U.int 1, integers [1, 2]]
  , Call PLC.LengthOfArray 1 [array]
  , Call PLC.ListToArray 1 [integers [1]]
  , Call PLC.IndexArray 1 [array, U.int 0]
  , Call PLC.Bls12_381_G1_multiScalarMul 0 [integers [2], U.forced PLC.MkCons [g1, U.con ([] :: [G1.Element])]]
  , Call PLC.Bls12_381_G2_multiScalarMul 0 [integers [2], U.forced PLC.MkCons [g2, U.con ([] :: [G2.Element])]]
  , Call PLC.InsertCoin 0 [U.bytes "\170", U.bytes "\170", U.int 1, value]
  , Call PLC.LookupCoin 0 [U.bytes "\170", U.bytes "\170", value]
  , Call PLC.UnionValue 0 [value, value]
  , Call PLC.ValueContains 0 [value, value]
  , Call PLC.ValueData 0 [value]
  , Call PLC.UnValueData 0 [valueData]
  , Call PLC.ScaleValue 0 [U.int 2, value]
  ]
  where
    hexBytes = U.bytes . either error id . B16.decode
    text = U.con :: Text -> U.T
    integers = U.con :: [Integer] -> U.T
    g1Bytes = hexBytes "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb"
    g2Bytes = hexBytes "93e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8"
    g1 = U.builtin PLC.Bls12_381_G1_uncompress [g1Bytes]
    g2 = U.builtin PLC.Bls12_381_G2_uncompress [g2Bytes]
    pairing = U.builtin PLC.Bls12_381_millerLoop [g1, g2]
    array = U.forced PLC.ListToArray [integers [1]]
    valueData = U.con (PV1.Map [(PV1.B "\170", PV1.Map [(PV1.B "\170", PV1.I 1)])])
    value = U.builtin PLC.UnValueData [valueData]

-- | Transactions that each reach a cell of the coverage table, with the verdict the ledger gives.
tableCells :: SpecWith (ImpInit (LedgerSpec E))
tableCells = describe "table cells" $ do
  forM_ ([Top, Sub] :: [Level]) $ \level -> do
    it ("auxiliary data holds V3 bytes that do not deserialise at the " <> show level) $ do
      let malformed = mkBasicTxAuxData & plutusScriptsTxAuxDataL .~ [(PlutusV3, PlutusBinary "invalid" :| [])]
          tx :: forall l. Typeable l => Tx l E
          tx = mkBasicTx mkBasicTxBody & auxDataTxL .~ SJust malformed
      void . expectLeft =<< trySubmitTx (case level of Top -> tx; Sub -> mkTopTxWithSubTxs [tx])
  it "a pool registration whose metadata hash has 33 bytes" $ do
    pool <- freshKeyHash
    params <- freshPoolParams pool =<< registerAccountAddress
    url <- arbitrary
    let metadata = PoolMetadata url (byteArrayFromList (replicate 33 (7 :: Word8)))
    void . expectLeft =<< trySubmitTx (mkBasicTx (mkBasicTxBody & certsTxBodyL .~ [RegPoolTxCert (params & sppMetadataL .~ SJust metadata)]) :: Tx TopTx E)
  it "a transaction with no key witness" $
    void . expectLeft =<< withPostFixup (pure . (witsTxL . addrTxWitsL .~ mempty)) (trySubmitTx (mkBasicTx mkBasicTxBody :: Tx TopTx E))
  it "a V3 spend whose redeemer declares zero units" $
    spendWithV3In (void . trySubmitTx) (ValidityInterval SNothing SNothing) (Data (PV1.I 0)) (ExUnits 0 0) "4901010023357369324d"
  forM_ ([Top, Sub] :: [Level]) $ \level -> do
    it ("a V4 reference script mints at the " <> show level) $ do
      (ref, hash) <- v4Reference
      holder <- freshKeyAddrNoPtr_
      let token = multiAssetFromList [(PolicyID hash, AssetName (toShort "a"), 1)]
      redeemsV4 level ref (MintingPurpose (AsIx 0)) (\b -> b & mintTxBodyL .~ token & outputsTxBodyL .~ [mkBasicTxOut holder (MaryValue (Coin 5_000_000) token)])
    it ("a V4 reference script votes as a committee hot credential at the " <> show level) $ do
      (ref, hash) <- v4Reference
      cold <- expectJust . listToMaybe . toList =<< getCommitteeMembers
      void $ registerCommitteeHotKeys (pure (ScriptHashObj hash)) (cold :| [])
      action <- submitGovAction InfoAction
      let votes = VotingProcedures [(CommitteeVoter (ScriptHashObj hash), [(action, VotingProcedure VoteYes SNothing)])]
      redeemsV4 level ref (VotingPurpose (AsIx 0)) (votingProceduresTxBodyL .~ votes)
    it ("a V4 reference script registers its stake credential at the " <> show level) $ do
      (ref, hash) <- v4Reference
      deposit <- getsPParams ppKeyDepositL
      redeemsV4 level ref (CertifyingPurpose (AsIx 0)) (certsTxBodyL .~ [RegDepositTxCert (ScriptHashObj hash) deposit])
    it ("a V4 reference script withdraws zero from its account at the " <> show level) $ do
      (ref, hash) <- v4Reference
      deposit <- getsPParams ppKeyDepositL
      redeemsV4 Top ref (CertifyingPurpose (AsIx 0)) (certsTxBodyL .~ [RegDepositTxCert (ScriptHashObj hash) deposit])
      network <- networkId <$> use impGlobalsL
      let account = AccountAddress network (AccountId (ScriptHashObj hash))
      redeemsV4 level ref (RewardingPurpose (AsIx 0)) (withdrawalsTxBodyL .~ Withdrawals [(account, Coin 0)])
    it ("a V4 reference script is the guardrail of a parameter change at the " <> show level) $ do
      (ref, hash) <- v4Reference
      modifyNES (newEpochStateGovStateL . constitutionGovStateL . constitutionGuardrailsScriptHashL .~ SJust hash)
      proposal <- mkProposal =<< mkParameterChangeGovAction SNothing (emptyPParamsUpdate & ppuPerasCertBoostL .~ SJust 7)
      redeemsV4 level ref (ProposingPurpose (AsIx 0)) (proposalProceduresTxBodyL .~ [proposal])
    it ("a V4 reference script is a guard at the " <> show level) $ do
      (ref, hash) <- v4Reference
      redeemsV4 level ref (GuardingPurpose (AsIx 0)) (guardsTxBodyL .~ [ScriptHashObj hash])
    it ("a hard fork initiation at the " <> show level) $ do
      version <- getProtVer
      previous <- getLastEnactedHardForkInitiation
      proposes level (HardForkInitiation previous (minorFollow version))
    it ("a treasury withdrawal with no guardrail script at the " <> show level) $ do
      dropGuardrail
      account <- registerAccountAddress
      proposes level =<< mkTreasuryWithdrawalsGovAction [(account, Coin 1_000_000)]
    it ("a no confidence motion at the " <> show level) $
      proposes level . NoConfidence =<< getLastEnactedCommittee
    it ("a vote delegation at the " <> show level) $ do
      staking <- registeredStaking
      drep <- registerDRep
      certifies level (DelegTxCert staking (DelegVote (DRepCredential (KeyHashObj drep))))
    it ("a stake and vote delegation at the " <> show level) $ do
      staking <- registeredStaking
      pool <- freshKeyHash
      registerPool pool
      drep <- registerDRep
      certifies level (DelegTxCert staking (DelegStakeVote pool (DRepCredential (KeyHashObj drep))))
    it ("a stake registration and delegation at the " <> show level) $ do
      staking <- KeyHashObj <$> freshKeyHash
      pool <- freshKeyHash
      registerPool pool
      deposit <- getsPParams ppKeyDepositL
      certifies level (RegDepositDelegTxCert staking (DelegStake pool) deposit)
    it ("a DRep update at the " <> show level) $ do
      drep <- registerDRep
      certifies level (UpdateDRepTxCert (KeyHashObj drep) SNothing)
    it ("a network id at the " <> show level) $ do
      network <- networkId <$> use impGlobalsL
      submitTx_ (atLevel level (mkBasicTxBody & networkIdTxBodyL .~ SJust network))
  it "guards written as bare key hashes at the top level" $ do
    guard <- KeyHashObj <$> freshKeyHash
    submitEdited noEdit {editBody = overKey 14 (everywhere bareKey)} (mkBasicTx (mkBasicTxBody & guardsTxBodyL .~ [guard]))
  it "guards written as bare key hashes at a sub transaction" $ do
    guard <- KeyHashObj <$> freshKeyHash
    subEdited (overKey 14 (everywhere bareKey)) (guardsTxBodyL .~ [guard]) id
  it "a parameter change proposes the current value of each parameter it names" $ do
    dropGuardrail
    pp <- getsPParams id
    proposes Top =<< mkParameterChangeGovAction SNothing (currentValues pp)
  it "a withdrawal account given twice" $ do
    account <- registerStakeCredential . KeyHashObj =<< freshKeyHash
    submitEdited noEdit {editBody = overKey 5 dupFirst} (mkBasicTx (mkBasicTxBody & withdrawalsTxBodyL .~ Withdrawals [(account, Coin 0)]))
  it "a bootstrap witness given twice in its set" $
    customBootstrapSpendWith (submitEdited noEdit {editWits = overKey 2 dupFirst}) Top (replicate 32 7) [0xa0]
  forM_ ([(4, "certificates"), (13, "collateral inputs"), (18, "reference inputs"), (20, "proposal procedures"), (23, "sub transactions")] :: [(Int, String)]) $ \(key, name) ->
    it ("an empty " <> name <> " set at body key " <> show key) $
      submitEdited noEdit {editBody = setKey key emptySet} (mkBasicTx mkBasicTxBody)
  it "an empty starting account balance intervals map" $
    submitEdited noEdit {editBody = setKey 27 (TMap [])} (mkBasicTx mkBasicTxBody)
  it "a sub transaction spends an output a V3 script locks" $ do
    script <- plutusScript PlutusV3 "4901010023357369324d"
    let hash = hashScript script
    locked <- sendCoinTo (Addr Testnet (ScriptHashObj hash) StakeRefNull) (Coin 5_000_000)
    collateral <- makeCollateralInput
    let sub :: Tx SubTx E
        sub =
          mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [locked])
            & witsTxL . scriptTxWitsL .~ [(hash, script)]
            & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (Data (PV1.I 0), v3Units))]
    void . expectLeft =<< trySubmitTx (mkTopTxWithSubTxs [sub] & bodyTxL . collateralInputsTxBodyL .~ [collateral])
  it "a V3 spend beside an account balance interval" $ do
    account <- registerStakeCredential . KeyHashObj =<< freshKeyHash
    spendWithV3Using (void . trySubmitTx . (bodyTxL . accountBalanceIntervalsTxBodyL .~ AccountBalanceIntervals [(account, AccountBalanceExact (Coin 0))])) "4901010023357369324d"
  it "a V3 spend beside a direct deposit" $ do
    account <- registerStakeCredential . KeyHashObj =<< freshKeyHash
    spendWithV3Using (void . trySubmitTx . (bodyTxL . directDepositsTxBodyL .~ DirectDeposits [(account, Coin 1_000_000)])) "4901010023357369324d"
  it "a V4 guard beside an output to a pointer address" $ do
    (ref, hash) <- v4Reference
    key <- freshKeyHash
    let pointer = Addr Testnet (KeyHashObj key) (StakeRefPtr (Ptr (SlotNo32 1) (mkTxIxPartial 2) (mkCertIxPartial 3)))
    redeemsV4With (void . trySubmitTx) Top ref (GuardingPurpose (AsIx 0)) (\b -> b & guardsTxBodyL .~ [ScriptHashObj hash] & outputsTxBodyL .~ [mkBasicTxOut pointer (inject (Coin 5_000_000))])
  it "a V4 guard at a sub transaction whose output is to a reward address" $ do
    (ref, hash) <- v4Reference
    holder <- freshKeyAddrNoPtr_
    let reward (TList (_ : rest)) = TList (TBytes (BS.cons 0xe0 (BS.replicate 28 5)) : rest)
        reward m = setKey 0 (TBytes (BS.cons 0xe0 (BS.replicate 28 5))) m
    redeemsV4With (submitEdited noEdit {editSubBody = firstSub (overKey 1 (overFirst reward))}) Sub ref (GuardingPurpose (AsIx 0)) (\b -> b & guardsTxBodyL .~ [ScriptHashObj hash] & outputsTxBodyL .~ [mkBasicTxOut holder (inject (Coin 5_000_000))])
  it "a V3 spend whose collateral return exceeds its collateral" $ do
    holder <- freshKeyAddrNoPtr_
    spendWithV3Using (void . trySubmitTx . (bodyTxL . collateralReturnTxBodyL .~ SJust (mkBasicTxOut holder (inject (Coin 1_000_000_000_000))))) "4901010023357369324d"
  it "a V1 script no purpose needs" $ do
    script <- fromPlutusScript <$> mkPlutusScript (alwaysSucceedsNoDatum SPlutusV1)
    void $ trySubmitTx (mkBasicTx mkBasicTxBody & witsTxL . scriptTxWitsL .~ [(hashScript script, script)] :: Tx TopTx E)
  it "a fee at a sub transaction body" $
    subEdited (setKey 2 (TInt 200_000)) id id
  it "a V3 spend whose redeemer pointer is given twice" $
    spendWithV3Using (submitEdited noEdit {editWits = overKey 5 dupFirst}) "4901010023357369324d"
  it "two outputs whose coins sum to two to the 64th, refused with a failure the ledger cannot decode" $
    submitEdited noEdit {editBody = overKey 1 (overIndex 1 halfWordCoin . overIndex 0 halfWordCoin . dupFirst)} (mkBasicTx mkBasicTxBody)
      `shouldThrow` (\e -> "MaryValue: expected array or int" `isInfixOf` show (e :: SomeException))

-- | An output term whose coin is two to the 63rd, in either output encoding.
halfWordCoin :: Term -> Term
halfWordCoin (TList (address : _ : rest)) = TList (address : TInteger (2 ^ (63 :: Int)) : rest)
halfWordCoin output = setKey 1 (TInteger (2 ^ (63 :: Int))) output

-- | A guard credential term written as the bare key hash it names, every other term unchanged.
bareKey :: Term -> Term
bareKey (TList [TInt 0, TBytes h]) = TBytes h
bareKey t = t

-- | A stake credential of a fresh key, registered.
registeredStaking :: ImpTestM E (Credential Staking)
registeredStaking = do
  staking <- KeyHashObj <$> freshKeyHash
  void (registerStakeCredential staking)
  pure staking

-- | Submits a transaction whose body at the level holds the certificate.
certifies :: Level -> TxCert E -> ImpTestM E ()
certifies level cert = submitTx_ (atLevel level (mkBasicTxBody & certsTxBodyL .~ [cert]))

-- | Submits a transaction whose body at the level proposes the action.
proposes :: Level -> GovAction E -> ImpTestM E ()
proposes level action = do
  proposal <- mkProposal action
  submitTx_ (atLevel level (mkBasicTxBody & proposalProceduresTxBodyL .~ [proposal]))

-- | A reference input whose output holds the V4 script that succeeds on any context, and the hash of that script.
v4Reference :: ImpTestM E (TxIn, ScriptHash)
v4Reference = do
  let plutus = alwaysSucceedsNoDatum SPlutusV4
  script <- fromPlutusScript <$> mkPlutusScript plutus
  holder <- freshKeyAddrNoPtr_
  made <- submitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [referenceOutput holder script]) :: Tx TopTx E)
  pure (txInAt 0 made, hashPlutusScript plutus)

-- | Submits a transaction whose body at the level, after the change, reads the reference input and redeems the purpose, with collateral at the top level.
redeemsV4 :: Level -> TxIn -> PlutusPurpose AsIx E -> (forall l. Typeable l => TxBody l E -> TxBody l E) -> ImpTestM E ()
redeemsV4 = redeemsV4With submitTx_

-- | Submits with the given submission a transaction whose body at the level, after the change, reads the reference input and redeems the purpose, with collateral at the top level.
redeemsV4With :: (Tx TopTx E -> ImpTestM E ()) -> Level -> TxIn -> PlutusPurpose AsIx E -> (forall l. Typeable l => TxBody l E -> TxBody l E) -> ImpTestM E ()
redeemsV4With submit level ref purpose change = do
  collateral <- makeCollateralInput
  let redeemed :: forall l. Typeable l => Tx l E
      redeemed =
        mkBasicTx (change (mkBasicTxBody & referenceInputsTxBodyL .~ [ref]))
          & witsTxL . rdmrsTxWitsL .~ Redeemers [(purpose, (Data (PV1.I 0), v3Units))]
      top = case level of
        Top -> redeemed
        Sub -> mkTopTxWithSubTxs [redeemed]
  submit (top & bodyTxL . collateralInputsTxBodyL .~ [collateral])

-- | A parameter update that proposes the current value of each parameter it names, and a header size of 1100 bytes where the current one is zero.
currentValues :: PParams E -> PParamsUpdate E
currentValues pp =
  emptyPParamsUpdate
    & ppuTxFeeFixedL .~ SJust (pp ^. ppTxFeeFixedL)
    & ppuMaxBBSizeL .~ SJust (pp ^. ppMaxBBSizeL)
    & ppuMaxTxSizeL .~ SJust (pp ^. ppMaxTxSizeL)
    & ppuMaxBHSizeL .~ SJust (max 1100 (pp ^. ppMaxBHSizeL))
    & ppuPoolDepositL .~ SJust (pp ^. ppPoolDepositL)
    & ppuEMaxL .~ SJust (pp ^. ppEMaxL)
    & ppuNOptL .~ SJust (pp ^. ppNOptL)
    & ppuA0L .~ SJust (pp ^. ppA0L)
    & ppuRhoL .~ SJust (pp ^. ppRhoL)
    & ppuTauL .~ SJust (pp ^. ppTauL)
    & ppuMinPoolCostL .~ SJust (pp ^. ppMinPoolCostL)
    & ppuPricesL .~ SJust (pp ^. ppPricesL)
    & ppuMaxTxExUnitsL .~ SJust (pp ^. ppMaxTxExUnitsL)
    & ppuMaxBlockExUnitsL .~ SJust (pp ^. ppMaxBlockExUnitsL)
    & ppuMaxValSizeL .~ SJust (pp ^. ppMaxValSizeL)
    & ppuCollateralPercentageL .~ SJust (pp ^. ppCollateralPercentageL)
    & ppuMaxCollateralInputsL .~ SJust (pp ^. ppMaxCollateralInputsL)
    & ppuCommitteeMaxTermLengthL .~ SJust (pp ^. ppCommitteeMaxTermLengthL)
    & ppuDRepActivityL .~ SJust (pp ^. ppDRepActivityL)
    & ppuMaxRefScriptSizePerBlockL .~ SJust (pp ^. ppMaxRefScriptSizePerBlockL)
    & ppuMaxRefScriptSizePerTxL .~ SJust (pp ^. ppMaxRefScriptSizePerTxL)
    & ppuRefScriptCostStrideL .~ SJust (pp ^. ppRefScriptCostStrideL)
    & ppuRefScriptCostMultiplierL .~ SJust (pp ^. ppRefScriptCostMultiplierL)
    & ppuMaxPledgeLeverageL .~ SJust (pp ^. ppMaxPledgeLeverageL)
    & ppuMinPoolMarginL .~ SJust (pp ^. ppMinPoolMarginL)
    & ppuLeiosAnnouncementPeriodLengthL .~ SJust (pp ^. ppLeiosAnnouncementPeriodLengthL)
    & ppuLeiosVotePeriodLengthL .~ SJust (pp ^. ppLeiosVotePeriodLengthL)
    & ppuLeiosDiffusionPeriodLengthL .~ SJust (pp ^. ppLeiosDiffusionPeriodLengthL)
    & ppuLeiosCommitteeSizeL .~ SJust (pp ^. ppLeiosCommitteeSizeL)
    & ppuLeiosQuorumStakeThresholdL .~ SJust (pp ^. ppLeiosQuorumStakeThresholdL)
    & ppuMaxEndorserBlockReferencesSizeL .~ SJust (pp ^. ppMaxEndorserBlockReferencesSizeL)
    & ppuMaxEndorserBlockTxsSizeL .~ SJust (pp ^. ppMaxEndorserBlockTxsSizeL)
    & ppuMaxEndorserBlockExUnitsL .~ SJust (pp ^. ppMaxEndorserBlockExUnitsL)
    & ppuMaxRefScriptSizePerEndorserBlockL .~ SJust (pp ^. ppMaxRefScriptSizePerEndorserBlockL)
    & ppuPerasMinCandidateBlockAgeL .~ SJust (pp ^. ppPerasMinCandidateBlockAgeL)
    & ppuPerasHealingFactorL .~ SJust (pp ^. ppPerasHealingFactorL)
    & ppuPerasTargetCommitteeSizeL .~ SJust (pp ^. ppPerasTargetCommitteeSizeL)
    & ppuPerasBootstrapRoundL .~ SJust (pp ^. ppPerasBootstrapRoundL)
    & ppuPerasQuorumThresholdSafetyMarginL .~ SJust (pp ^. ppPerasQuorumThresholdSafetyMarginL)
    & ppuRefInputsCostPerMultiAssetPolicyL .~ SJust (pp ^. ppRefInputsCostPerMultiAssetPolicyL)
    & ppuRefInputsCostPerDatumByteL .~ SJust (pp ^. ppRefInputsCostPerDatumByteL)

-- | An integer term squared the given number of times.
squared :: Int -> U.T -> U.T
squared n x = iterate (U.apply (U.lam (U.builtin PLC.MultiplyInteger [U.var, U.var]))) x !! n

-- | Transactions whose integers, scripts, arrays and proposals have shapes the read back and evaluation paths decode or map, each case with the verdict the ledger gives.
readBackShapes :: SpecWith (ImpInit (LedgerSpec E))
readBackShapes = describe "read back shapes" $ do
  it "an output holds an inline datum of integers outside 64 bits" $ do
    holder <- freshKeyAddrNoPtr_
    let wide = Data (PV1.List (map PV1.I [2 ^ (63 :: Int), 2 ^ (64 :: Int) - 1, -(2 ^ (63 :: Int)) - 1, -(2 ^ (64 :: Int))])) :: Data E
        out = mkBasicTxOut holder (inject (Coin 5_000_000)) & datumTxOutL .~ Datum (dataToBinaryData wide)
    void $ trySubmitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]) :: Tx TopTx E)
  it "auxiliary data holds a V3 script" $ do
    binary <- expectJust (either (const Nothing) Just (B16.decode "4901010023357369324d"))
    let aux = mkBasicTxAuxData & plutusScriptsTxAuxDataL .~ [(PlutusV3, PlutusBinary (toShort binary) :| [])]
    void $ trySubmitTx (mkBasicTx mkBasicTxBody & auxDataTxL .~ SJust aux :: Tx TopTx E)
  it "a stake registration whose credential is an indefinite length array" $ do
    key <- KeyHashObj <$> freshKeyHash
    deposit <- getsPParams ppKeyDepositL
    submitEdited noEdit {editBody = overKey 4 (overFirst (overIndex 1 indefiniteArray))} (mkBasicTx (mkBasicTxBody & certsTxBodyL .~ [RegDepositTxCert key deposit]))
  it "an output whose value is an indefinite length array" $ do
    policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
    holder <- freshKeyAddrNoPtr_
    let token = multiAssetFromList [(policy, AssetName (toShort "a"), 1)]
        out = mkBasicTxOut holder (MaryValue (Coin 5_000_000) token)
        value f o = case o of
          TList (a : v : rest) -> TList (a : f v : rest)
          m -> overKey 1 f m
    submitEdited noEdit {editBody = overKey 1 (overFirst (value indefiniteArray))} (mkBasicTx (mkBasicTxBody & mintTxBodyL .~ token & outputsTxBodyL .~ [out]))
  it "a V3 spend beside a parameter change that proposes a Peras parameter with no guardrail script" $ do
    dropGuardrail
    proposal <- mkProposal =<< mkParameterChangeGovAction SNothing (emptyPParamsUpdate & ppuPerasCertBoostL .~ SJust 7)
    spendWithV3Using (void . trySubmitTx . (bodyTxL . proposalProceduresTxBodyL .~ [proposal])) "4901010023357369324d"
  it "a V3 spend beside a parameter change that proposes a cost model of an unknown language with no guardrail script" $ do
    dropGuardrail
    models <- mkCostModelsLenient [(9, [1, 2, 3])]
    proposal <- mkProposal =<< mkParameterChangeGovAction SNothing (emptyPParamsUpdate & ppuCostModelsL .~ SJust models)
    spendWithV3Using (void . trySubmitTx . (bodyTxL . proposalProceduresTxBodyL .~ [proposal])) "4901010023357369324d"
  it "a V3 spend beside a new constitution with no guardrail script" $ do
    anchor <- arbitrary
    previous <- getLastEnactedConstitution
    proposal <- mkProposal (NewConstitution previous (Constitution anchor SNothing))
    spendWithV3Using (void . trySubmitTx . (bodyTxL . proposalProceduresTxBodyL .~ [proposal])) "4901010023357369324d"
  it "a V3 spend whose transaction pays a bootstrap address" $ do
    boot <- freshBootstrapAddress
    let out = mkBasicTxOut (AddrBootstrap boot) (inject (Coin 2_000_000))
    spendWithV3Using (void . trySubmitTx . (bodyTxL . outputsTxBodyL %~ (<> [out]))) "4901010023357369324d"

-- | A list term written as an indefinite length array.
indefiniteArray :: Term -> Term
indefiniteArray (TList xs) = TListI xs
indefiniteArray t = t

-- | Removes the guardrail script from the constitution, so a proposal names no policy and runs no script.
dropGuardrail :: ImpTestM E ()
dropGuardrail = modifyNES (newEpochStateGovStateL . constitutionGovStateL . constitutionGuardrailsScriptHashL .~ SNothing)

-- | Spends at a validator that succeeds when the transaction's validity range equals its redeemer, each case with the range the ledger is expected to build.
validityBounds :: SpecWith (ImpInit (LedgerSpec E))
validityBounds = describe "validity bounds" $ do
  it "a V3 script sees an exclusive upper bound" $ do
    g <- use impGlobalsL
    SlotNo s <- use impCurSlotNoG
    let u = SlotNo (s + 10)
    spendInRange submitTx_ (ValidityInterval SNothing (SJust u)) (PV1.Interval (PV1.LowerBound PV1.NegInf True) (PV1.strictUpperBound (posixAt g u)))
  it "a V3 script sees a closed lower bound" $ do
    g <- use impGlobalsL
    s <- use impCurSlotNoG
    spendInRange submitTx_ (ValidityInterval (SJust s) SNothing) (PV1.Interval (PV1.lowerBound (posixAt g s)) (PV1.UpperBound PV1.PosInf True))
  it "a V3 script with its upper bound at the slot before the forecast horizon" $
    uptoHorizon 1 submitTx_
  it "a V3 script with its upper bound at the forecast horizon" $
    uptoHorizon 0 (void . expectLeft <=< trySubmitTx)

-- (program 1.1.0 (lam ctx [(lam fields [(lam info (force [[[(force (builtin ifThenElse)) [[(builtin equalsData) [(force (builtin headList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (builtin tailList)) [(force (force (builtin sndPair))) [(builtin unConstrData) info]]]]]]]]]]] [(force (builtin headList)) [(force (builtin tailList)) fields]]]] (delay (con unit ()))] (delay (error))])) [(force (builtin headList)) fields]]) [(force (force (builtin sndPair))) [(builtin unConstrData) ctx]]]))

-- | A V3 validator that succeeds when its transaction's validity range equals its redeemer.
rangeIsRedeemer :: ByteString
rangeIsRedeemer = "5839010100232325333573466ebcd5d09aba2357446ae88d5d11aba2357446ae88d55cf1baa001357426ae8800852616357420026aae78dd500081"

-- | Submits a spend at the range checking validator whose upper bound is the given number of slots before the forecast horizon of a node validating at the current slot, under globals that translate no slot from that horizon on.
uptoHorizon :: Word64 -> (Tx TopTx E -> ImpTestM E ()) -> ImpTestM E ()
uptoHorizon before submit = do
  g <- use impGlobalsL
  let fixed = epochInfoPure g
      window = stabilityWindow g
      size at = unEpochSize (runIdentity (epochInfoEpoch fixed at >>= epochInfoSize fixed))
  SlotNo s0 <- use impCurSlotNoG
  when ((s0 + window) `mod` size (SlotNo s0) == 0) passTick
  current <- use impCurSlotNoG
  let SlotNo s = current
      horizon = SlotNo (((s + window) `div` size current + 1) * size current)
      u = SlotNo (unSlotNo horizon - before)
  impGlobalsL %= bounded horizon
  spendInRange submit (ValidityInterval SNothing (SJust u)) (PV1.Interval (PV1.LowerBound PV1.NegInf True) (PV1.strictUpperBound (posixAt g u)))

-- | The globals with an epoch info that translates no slot at or past the horizon.
bounded :: SlotNo -> Globals -> Globals
bounded horizon g = g {epochInfo = ei {epochInfoSlotToRelativeTime_ = translate}}
  where
    ei = epochInfo g
    translate s
      | s >= horizon = Left "past the forecast horizon"
      | otherwise = epochInfoSlotToRelativeTime_ ei s

-- | The POSIX time of a slot at the system start and slot length of the globals.
posixAt :: Globals -> SlotNo -> PV1.POSIXTime
posixAt g s = PV1.POSIXTime (floor (utcTimeToPOSIXSeconds start * 1000) + toInteger (unSlotNo s) * slotLengthToMillisec (runIdentity (epochInfoSlotLength (epochInfoPure g) s)))
  where
    SystemStart start = systemStart g

-- | Locks coin at the range checking validator, then submits a spend of it with the validity interval and the range as its redeemer.
spendInRange :: (Tx TopTx E -> ImpTestM E ()) -> ValidityInterval -> PV1.POSIXTimeRange -> ImpTestM E ()
spendInRange submit interval range = spendWithV3In submit interval (Data (PV1.toData range)) v3Units rangeIsRedeemer

-- | Spends whose script context holds a hash, credential or reference the ledger builds from the transaction, each case with the value the ledger is expected to build or the refusal it is expected to give.
contextValues :: SpecWith (ImpInit (LedgerSpec E))
contextValues = describe "context values" $ do
  it "a V3 script sees a reference input holding a native script with a guard" $ do
    guard <- KeyHashObj <$> freshKeyHash
    seesReferenceScript PlutusV3 (fromNativeScript (RequireGuard guard))
  it "a V3 script sees a reference input holding a native script with a nested guard" $ do
    guard <- KeyHashObj <$> freshKeyHash
    seesReferenceScript PlutusV3 (fromNativeScript (RequireAllOf [RequireGuard guard]))
  it "a V3 script sees a reference input holding a native script written with nonminimal lengths" $ do
    holder <- freshKeyAddrNoPtr_
    script <- fromNativeScript . RequireSignature <$> freshKeyHash
    let nonminimal = TTagged 24 (TBytes "\x82\x00\x98\x02\x18\x01\x80")
    made <- withPostFixup (editTx noEdit {editBody = overKey 1 (overFirst (overKey 3 (const nonminimal)))}) (submitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [referenceOutput holder script])))
    let ref = txInAt 0 made
    held <- expectJust . strictMaybeToMaybe . (^. referenceScriptTxOutL) =<< impGetUTxO ref
    hashScript held `shouldNotBe` hashScript script
    walkSpend PlutusV3 [0, 1, 0, 1, 3] (PV1.toData (Just (transScriptHash (hashScript held)))) (bodyTxL . referenceInputsTxBodyL .~ [ref])
  it "a V2 script sees a reference input holding a V3 script" $
    seesReferenceScript PlutusV2 =<< plutusScript PlutusV3 "4901010023357369324d"
  it "a V3 script sees a reference input holding a V4 script" $
    seesReferenceScript PlutusV3 =<< plutusScript PlutusV4 "450101004981"
  it "a V1 script sees an output holding a reference script" $ do
    holder <- freshKeyAddrNoPtr_
    script <- plutusScript PlutusV3 "4901010023357369324d"
    address <- expectJust (transAddr holder)
    let out = PV1.TxOut address (PV1.singleton PV1.adaSymbol PV1.adaToken 20_000_000) Nothing
    walkSpend PlutusV1 [0, 1, 0] (PV1.toData out) (bodyTxL . outputsTxBodyL .~ [referenceOutput holder script])
  it "a V3 script sees an output to a pointer address" $ do
    key <- freshKeyHash
    seesOutputAddress (Addr Testnet (KeyHashObj key) (StakeRefPtr (Ptr (SlotNo32 1) (mkTxIxPartial 2) (mkCertIxPartial 3))))
  it "a V3 script sees an output to a script address with a key stake credential" $ do
    script <- impAddNativeScript . RequireSignature =<< freshKeyHash
    stake <- freshKeyHash
    seesOutputAddress (Addr Testnet (ScriptHashObj script) (StakeRefBase (KeyHashObj stake)))
  it "a V3 script sees guards given in descending order" $ do
    a <- freshKeyHash
    b <- freshKeyHash
    let (low, high) = (min a b, max a b)
    walkSpend PlutusV3 [0, 8] (PV1.toData ([transKeyHash low, transKeyHash high] :: [PV1.PubKeyHash])) (bodyTxL . guardsTxBodyL .~ [KeyHashObj high, KeyHashObj low])
  it "a V3 script sees withdrawals from a key and a script credential" $ do
    key <- KeyHashObj <$> freshKeyHash
    script <- ScriptHashObj <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
    keyAccount <- registerStakeCredential key
    scriptAccount <- registerStakeCredential script
    let withdrawn = PV1.Map [(PV1.toData (transCred script), PV1.I 0), (PV1.toData (transCred key), PV1.I 0)]
    walkSpend PlutusV3 [0, 6] withdrawn (bodyTxL . withdrawalsTxBodyL .~ Withdrawals [(keyAccount, Coin 0), (scriptAccount, Coin 0)])
  it "a V2 script sees a stake registration certificate" $ do
    key <- KeyHashObj <$> freshKeyHash
    deposit <- getsPParams ppKeyDepositL
    walkSpend PlutusV2 [0, 5, 0] (PV1.toData (PV1.DCertDelegRegKey (PV1.StakingHash (transCred key)))) (bodyTxL . certsTxBodyL .~ [RegDepositTxCert key deposit])
  it "a V3 script spends with a native script guard" $ do
    guard <- impAddNativeScript . RequireSignature =<< freshKeyHash
    spendWithV3Using (void . expectLeft <=< trySubmitTx . (bodyTxL . guardsTxBodyL .~ [ScriptHashObj guard])) "4901010023357369324d"
  it "a V3 script sees a stake registration certificate" $ do
    key <- KeyHashObj <$> freshKeyHash
    deposit <- getsPParams ppKeyDepositL
    walkSpend PlutusV3 [0, 5, 0] (PV3.toData (PV3.TxCertRegStaking (transCred key) (Just (transCoinToLovelace deposit)))) (bodyTxL . certsTxBodyL .~ [RegDepositTxCert key deposit])
  it "a V2 script sees a withdrawal from a key credential" $ do
    key <- KeyHashObj <$> freshKeyHash
    account <- registerStakeCredential key
    walkSpend PlutusV2 [0, 6] (PV1.Map [(PV1.toData (PV1.StakingHash (transCred key)), PV1.I 0)]) (bodyTxL . withdrawalsTxBodyL .~ Withdrawals [(account, Coin 0)])
  it "a V3 script spends with a required top-level guard" $ do
    guard <- KeyHashObj <$> freshKeyHash
    spendWithV3Using (void . expectLeft <=< trySubmitTx . (bodyTxL . guardsTxBodyL .~ [guard]) . (bodyTxL . requiredTopLevelGuardsTxBodyL .~ [(guard, SNothing)])) "4901010023357369324d"

-- (program 1.1.0 [(lam nth [(lam step [(lam walk [(lam check (lam ctx [[check [(force (builtin headList)) [(force (builtin tailList)) [(force (force (builtin sndPair))) [(builtin unConstrData) ctx]]]]] ctx])) (lam r (lam ctx [(lam fields (force [[[(force (builtin ifThenElse)) [[(builtin equalsData) [[walk ctx] [(builtin unListData) [(force (builtin headList)) fields]]]] [(force (builtin headList)) [(force (builtin tailList)) fields]]]] (delay (con unit ()))] (delay (error))])) [(force (force (builtin sndPair))) [(builtin unConstrData) r]]]))]) [(lam f [(lam x [f (lam v [[x x] v])]) (lam x [f (lam v [[x x] v])])]) (lam walk (lam d (lam path (force [[[(force (force (builtin chooseList))) path] (delay d)] (delay [[walk [[step d] [(builtin unIData) [(force (builtin headList)) path]]]] [(force (builtin tailList)) path]])]))))]]) (lam d (lam k (force [[[[[[(force (builtin chooseData)) d] (delay [[nth [(force (force (builtin sndPair))) [(builtin unConstrData) d]]] k])] (delay [(lam p [(builtin listData) [[(force (builtin mkCons)) [(force (force (builtin fstPair))) p]] [[(force (builtin mkCons)) [(force (force (builtin sndPair))) p]] [(builtin mkNilData) (con unit ())]]]]) [[nth [(builtin unMapData) d]] k]])] (delay [[nth [(builtin unListData) d]] k])] (delay (error))] (delay (error))])))]) [(lam f [(lam x [f (lam v [[x x] v])]) (lam x [f (lam v [[x x] v])])]) (lam nth (lam xs (lam k (force [[[(force (builtin ifThenElse)) [[(builtin equalsInteger) k] (con integer 0)]] (delay [(force (builtin headList)) xs])] (delay [[nth [(force (builtin tailList)) xs]] [[(builtin subtractInteger) k] (con integer 1)]])]))))]])
-- The V1 and V2 validator is the same program at version 1.0.0 whose check takes the datum, the redeemer and the context as three arguments.

-- | The validator of a language that succeeds when the value its redeemer's path reaches in the script context equals the value its redeemer names, as CBOR hex.
walker :: Language -> ByteString
walker PlutusV1 = walkerV1V2
walker PlutusV2 = walkerV1V2
walker PlutusV3 = "58cd01010032323232233002357426ae88d55cf1baa00100122325333573466ebccc010008dd61aba1001357426ae880045261635573c6ea8008c8c8c0088cc0080080048c0088cc0080080048894ccd55cf8008801099801998020011bad357420026ae88004894cccccd5d20010998019aab9e37540040022646e9ccd5d01aab9d0013357406aae78004dd8a4c660066eac0080044cc00cdd60010008b0b19191801119801001000918011198010010009112999ab9a3370e002900009aba10021330033574400466e0400520021"
walker PlutusV4 = error "a V4 walking validator is not written"

-- | The walking validator of V1 and V2, as CBOR hex.
walkerV1V2 :: ByteString
walkerV1V2 = "58c5010000323232322223300400200122325333573466ebccc010008dd61aba1001357426ae880045261635573c6ea8008c8c8c0088cc0080080048c0088cc0080080048894ccd55cf8008801099801998020011bad357420026ae88004894cccccd5d20010998019aab9e37540040022646e9ccd5d01aab9d0013357406aae78004dd8a4c660066eac0080044cc00cdd60010008b0b19191801119801001000918011198010010009112999ab9a3370e002900009aba10021330033574400466e04005200201"

-- | Locks coin under a datum hash at the walking validator of a language, then spends it, after the change to the spend, with the path into the script context and the value expected there as its redeemer.
walkSpend :: Language -> [Integer] -> PV1.Data -> (Tx TopTx E -> Tx TopTx E) -> ImpTestM E ()
walkSpend language path expected change = do
  script <- plutusScript language (walker language)
  let hash = hashScript script
      datum = Data (PV1.I 0) :: Data E
      redeemer = Data (PV1.Constr 0 [PV1.List (map PV1.I path), expected]) :: Data E
      out = mkBasicTxOut (Addr Testnet (ScriptHashObj hash) StakeRefNull) (inject (Coin 50_000_000)) & datumTxOutL .~ DatumHash (hashData datum)
  made <- submitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]) :: Tx TopTx E)
  collateral <- makeCollateralInput
  submitTx_ . change $
    mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [txInAt 0 made] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 400_000)
      & witsTxL . scriptTxWitsL .~ [(hash, script)]
      & witsTxL . datsTxWitsL .~ TxDats [(hashData datum, datum)]
      & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (redeemer, ExUnits 1_000_000 1_000_000_000))]

-- | Spends at the walking validator of a language with a reference input whose output holds the script, expecting the context to give the script's hash.
seesReferenceScript :: Language -> Script E -> ImpTestM E ()
seesReferenceScript language script = do
  holder <- freshKeyAddrNoPtr_
  made <- submitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [referenceOutput holder script]) :: Tx TopTx E)
  walkSpend language [0, 1, 0, 1, 3] (PV1.toData (Just (transScriptHash (hashScript script)))) (bodyTxL . referenceInputsTxBodyL .~ [txInAt 0 made])

-- | Spends at the V3 walking validator with a first output to the address, expecting the context to give the address the ledger builds.
seesOutputAddress :: Addr -> ImpTestM E ()
seesOutputAddress address = do
  expected <- expectJust (transAddr address)
  walkSpend PlutusV3 [0, 2, 0, 0] (PV1.toData expected) (bodyTxL . outputsTxBodyL .~ [mkBasicTxOut address (inject (Coin 5_000_000))])

-- | An output to the address holding the script as its reference script.
referenceOutput :: Addr -> Script E -> TxOut E
referenceOutput holder script = mkBasicTxOut holder (inject (Coin 20_000_000)) & referenceScriptTxOutL .~ SJust script

-- | The Plutus script of a language given as its CBOR hex.
plutusScript :: Language -> ByteString -> ImpTestM E (Script E)
plutusScript language hex = do
  plutus :: PlutusScript E <- expectJust $ mkBinaryPlutusScript language . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode hex)
  pure (fromPlutusScript plutus)

-- | Transactions whose bytes break one rule of the node's decoder, or one it reads differently from the CDDL, each case at a slot of its own.
decoderRules :: SpecWith (ImpInit (LedgerSpec E))
decoderRules = describe "decoder rules" $
  forM_ (zip [1 ..] cases) $ \(ticks, (name, submit)) ->
    it name (replicateM_ ticks passTick >> submit)
  where
    cases :: [(String, ImpTestM E ())]
    cases =
      [ ("an empty guards set at body key 14 at the top level", edited (body (setKey 14 emptySet)))
      , ("an empty guards set at body key 14 at a sub transaction", subEdited (setKey 14 emptySet) id id)
      , ( "an inline datum under tag 24 over indefinite length bytes"
        , do
            out <- inlineDatumOutput
            submitEdited (body (overKey 1 (overFirst (overKey 2 (overIndex 1 indefinite))))) (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]))
        )
      , ("a key witness given twice in its set", edited (wits (overKey 0 dupFirst)))
      , ( "a datum given twice in its set"
        , do
            holder <- freshKeyAddrNoPtr_
            let datum = Data (PV1.I 7) :: Data E
                out = mkBasicTxOut holder (inject (Coin 5_000_000)) & datumTxOutL .~ DatumHash (hashData datum)
            submitEdited (wits (overKey 4 dupFirst)) (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]) & witsTxL . datsTxWitsL .~ TxDats [(hashData datum, datum)])
        )
      , ("an empty required top level guards map at a sub transaction", subEdited (setKey 24 (TMap [])) id id)
      , ( "a guard given twice at body key 14"
        , do
            guard <- KeyHashObj <$> freshKeyHash
            submitEdited (body (overKey 14 dupFirst)) (mkBasicTx (mkBasicTxBody & guardsTxBodyL .~ [guard]))
        )
      , ("an empty redeemers map", edited (wits (setKey 5 (TMap []))))
      , ( "redeemer memory units past the largest signed 64 bit integer"
        , spendWithV3Using (submitEdited (wits (overKey 5 (overFirstValue (overIndex 1 (overIndex 0 (const (TInteger (2 ^ (63 :: Int)))))))))) "4901010023357369324d"
        )
      , ("an empty withdrawals map", edited (body (setKey 5 (TMap []))))
      , ("an output address of one byte", edited (body (overKey 1 (overFirst (outAddr (TBytes "\x60"))))))
      , ("a byte after the transaction", submitEdited noEdit {trailing = "\x00"} plain)
      , ("a witness set key given twice", edited (wits (dupKey 0)))
      , ("a witness set key the era does not define", edited (wits (addEntry (TInt 99) (TInt 0))))
      , ("a verification key of 31 bytes", edited (wits (overKey 0 (overFirst (overIndex 0 (shorten 31))))))
      , ("a signature of 63 bytes", edited (wits (overKey 0 (overFirst (overIndex 1 (shorten 63))))))
      , ("a body key given twice", edited (body (dupKey 2)))
      , ("a body key the era does not define", edited (body (addEntry (TInt 99) (TInt 0))))
      , ("an input given twice in its set", edited (body (overKey 0 dupFirst)))
      , ("an empty votes map", edited (body (setKey 19 (TMap []))))
      , ("a donation of zero", edited (body (setKey 22 (TInt 0))))
      , ("an empty mint map", edited (body (setKey 9 (TMap []))))
      , ("a mint policy with no assets", edited (body (setKey 9 (TMap [(policyBytes, TMap [])]))))
      , ( "a minted asset name of 33 bytes"
        , do
            policy <- PolicyID <$> (impAddNativeScript . RequireSignature =<< freshKeyHash)
            holder <- freshKeyAddrNoPtr_
            let name = BS.replicate 32 1
                token = multiAssetFromList [(policy, AssetName (toShort name), 1)]
                longer (TBytes b) | b == name = TBytes (name <> "\x01")
                longer t = t
            submitEdited (body (everywhere longer)) (mkBasicTx (mkBasicTxBody & mintTxBodyL .~ token & outputsTxBodyL .~ [mkBasicTxOut holder (MaryValue (Coin 5_000_000) token)]))
        )
      , ("an output asset quantity of zero", edited (body (overKey 1 (overFirst (outValue (withAssets (TMap [(TBytes "a", TInt 0)])))))))
      , ("an output policy with no assets", edited (body (overKey 1 (overFirst (outValue (withAssets (TMap [])))))))
      , ( "an output map key given twice"
        , do
            out <- inlineDatumOutput
            submitEdited (body (overKey 1 (overFirst (dupKey 0)))) (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]))
        )
      , ( "metadata bytes of 65 bytes"
        , void $ trySubmitTx (plain & auxDataTxL .~ SJust (mkBasicTxAuxData & metadataTxAuxDataL .~ [(1, B (byteArrayFromList (replicate 65 (1 :: Word8))))]))
        )
      , ("an auxiliary data map key given twice", submitEdited noEdit {editAux = dupAuxKey} (plain & auxDataTxL .~ SJust (mkBasicTxAuxData & metadataTxAuxDataL .~ [(1, I 1)])))
      , ( "a required top level guard given twice at a sub transaction"
        , do
            guard <- KeyHashObj <$> freshKeyHash
            subEdited (overKey 24 dupFirst) (requiredTopLevelGuardsTxBodyL .~ [(guard, SNothing)]) (guardsTxBodyL .~ [guard])
        )
      , ("an empty direct deposits map", edited (body (setKey 25 (TMap []))))
      , ("an empty account balance intervals map", edited (body (setKey 26 (TMap []))))
      , ("a bootstrap witness with a 33 byte chain code at the top level", customBootstrapSpend Top (replicate 33 7) [0xa0])
      , ( "a guard given twice at body key 14 at a sub transaction"
        , do
            guard <- KeyHashObj <$> freshKeyHash
            subEdited (overKey 14 dupFirst) (guardsTxBodyL .~ [guard]) id
        )
      , ( "an output script reference under tag 24 over indefinite length bytes"
        , do
            out <- scriptRefOutput
            submitEdited (body (overKey 1 (overFirst (overKey 3 indefinite)))) (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]))
        )
      ]
    plain :: Tx TopTx E
    plain = mkBasicTx mkBasicTxBody
    edited e = submitEdited e plain
    body f = noEdit {editBody = f}
    wits f = noEdit {editWits = f}
    policyBytes = TBytes (BS.replicate 28 9)
    withAssets assets coin = TList [coin, TMap [(policyBytes, assets)]]
    shorten n (TBytes b) = TBytes (BS.take n b)
    shorten _ t = t
    indefinite (TTagged 24 (TBytes b)) = TTagged 24 (TBytesI (BL.fromStrict b))
    indefinite t = t
    outValue f out = case out of
      TList (a : v : rest) -> TList (a : f v : rest)
      m -> overKey 1 f m
    outAddr new out = case out of
      TList (_ : rest) -> TList (new : rest)
      m -> setKey 0 new m
    dupAuxKey (TTagged n m) = TTagged n (dupKey 0 m)
    dupAuxKey m = dupFirst m

-- | An output to a fresh key address holding an inline datum.
inlineDatumOutput :: ImpTestM E (TxOut E)
inlineDatumOutput = do
  holder <- freshKeyAddrNoPtr_
  pure (mkBasicTxOut holder (inject (Coin 5_000_000)) & datumTxOutL .~ Datum (dataToBinaryData (Data (PV1.I 42) :: Data E)))

-- | An output to a fresh key address holding a V3 script as its reference script.
scriptRefOutput :: ImpTestM E (TxOut E)
scriptRefOutput = do
  holder <- freshKeyAddrNoPtr_
  plutus :: PlutusScript E <- expectJust $ mkBinaryPlutusScript PlutusV3 . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode "4901010023357369324d")
  pure (mkBasicTxOut holder (inject (Coin 20_000_000)) & referenceScriptTxOutL .~ SJust (fromPlutusScript plutus))

-- | Submits a top level transaction with one sub transaction that spends a fresh input, after the edit to the sub transaction body.
subEdited :: (Term -> Term) -> (TxBody SubTx E -> TxBody SubTx E) -> (TxBody TopTx E -> TxBody TopTx E) -> ImpTestM E ()
subEdited f subBody topBody = do
  x <- freshFundedTxIn
  let sub = mkBasicTx (subBody (mkBasicTxBody & inputsTxBodyL .~ [x])) :: Tx SubTx E
  submitEdited noEdit {editSubBody = firstSub f} (mkTopTxWithSubTxs [sub] & bodyTxL %~ topBody)

-- | Where a body goes in a submitted transaction.
data Level = Top | Sub

instance Show Level where
  show Top = "top level"
  show Sub = "sub transaction"

-- | A top level transaction with the body, or with one sub transaction that has the body.
atLevel :: Level -> (forall l. Typeable l => TxBody l E) -> Tx TopTx E
atLevel Top body = mkBasicTx body
atLevel Sub body = mkTopTxWithSubTxs [mkBasicTx body]

-- | Runs an action whose submitted transaction loses the bootstrap witnesses of the level after every fixup.
withoutBootstrapWitnesses :: Level -> ImpTestM E a -> ImpTestM E a
withoutBootstrapWitnesses Top = withPostFixup (pure . (witsTxL . bootAddrTxWitsL .~ mempty))
withoutBootstrapWitnesses Sub = withPostFixupSubTxs (pure . (witsTxL . bootAddrTxWitsL .~ mempty))

-- | Submits a spend of a fresh bootstrap input at the level, whose witness carries the given chain code and attribute bytes.
customBootstrapSpend :: Level -> [Word8] -> [Word8] -> ImpTestM E ()
customBootstrapSpend level = customBootstrapSpendWith submitTx_ level

-- | Submits with the given submission a spend of a fresh bootstrap input at the level, whose witness carries the given chain code and attribute bytes.
customBootstrapSpendWith :: (Tx TopTx E -> ImpTestM E ()) -> Level -> [Word8] -> [Word8] -> ImpTestM E ()
customBootstrapSpendWith submit level chainCode attributes = do
  ByronKeyPair _ signingKey <- uniformM
  let attrs = Byron.mkAttributes (Byron.AddrAttributes Nothing (Byron.NetworkTestnet 0))
      witness :: SafeHash EraIndependentTxBody -> BootstrapWitness
      witness body =
        (makeBootstrapWitness (extractHash body) signingKey attrs)
          { bwChainCode = ChainCode (byteArrayFromList chainCode)
          , bwAttributes = byteArrayFromList attributes
          }
      placeholder = witness (hashAnnotated (mkBasicTxBody :: TxBody TopTx E))
      KeyHash root = bootstrapWitKeyHash placeholder
      addr = BootstrapAddress (Byron.Address (unsafeAbstractHashFromBytes (hashToBytes root)) attrs Byron.ATVerKey)
      signed :: forall l. Tx l E -> ImpTestM E (Tx l E)
      signed tx = pure (tx & witsTxL . bootAddrTxWitsL .~ [witness (hashAnnotated (tx ^. bodyTxL))])
  bootIn <- sendCoinTo (AddrBootstrap addr) mempty
  let spend :: forall l. Typeable l => Tx l E
      spend = mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [bootIn]) & witsTxL . bootAddrTxWitsL .~ [placeholder]
  case level of
    Top -> withPostFixup signed (submit (spend :: Tx TopTx E))
    Sub -> withPostFixupSubTxs signed (submit (mkTopTxWithSubTxs [spend]))

-- | Locks coin at a V3 script given as its CBOR hex, then spends it with that script and no datum.
spendWithV3 :: ByteString -> ImpTestM E ()
spendWithV3 = spendWithV3Using submitTx_

-- | Locks coin at a V3 script given as its CBOR hex, then submits a spend of it with that script and no datum.
spendWithV3Using :: (Tx TopTx E -> ImpTestM E ()) -> ByteString -> ImpTestM E ()
spendWithV3Using submit = spendWithV3In submit (ValidityInterval SNothing SNothing) (Data (PV1.I 0)) v3Units

-- | The units the redeemer of a V3 spend declares.
v3Units :: ExUnits
v3Units = ExUnits 1_000_000 1_000_000_000

-- | Locks coin at a V3 script given as its CBOR hex, then submits a spend of it with that script, the validity interval, the redeemer and its units, and no datum.
spendWithV3In :: (Tx TopTx E -> ImpTestM E ()) -> ValidityInterval -> Data E -> ExUnits -> ByteString -> ImpTestM E ()
spendWithV3In submit interval redeemer units hex = do
  plutus :: PlutusScript E <- expectJust $ mkBinaryPlutusScript PlutusV3 . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode hex)
  let script = fromPlutusScript plutus
      hash = hashScript script
  locked <- sendCoinTo (Addr Testnet (ScriptHashObj hash) StakeRefNull) (Coin 5_000_000)
  collateral <- makeCollateralInput
  let tx :: Tx TopTx E
      tx =
        mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [locked] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 400_000 & vldtTxBodyL .~ interval)
          & witsTxL . scriptTxWitsL .~ [(hash, script)]
          & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (redeemer, units))]
  submit tx

-- | Sets the cost models to the registered ones, then spends at a script of a language that calls the builtin.
callSpend :: (Tx TopTx E -> ImpTestM E ()) -> Language -> U.T -> ImpTestM E ()
callSpend submit language call = do
  models <- mkCostModelsLenient =<< liftIO registeredCostModels
  modifyPParams (ppCostModelsL .~ models)
  spendCalling submit language call

-- | Locks coin under a datum hash at the script of a language whose body ignores the result of the call, then submits a spend of it.
spendCalling :: (Tx TopTx E -> ImpTestM E ()) -> Language -> U.T -> ImpTestM E ()
spendCalling submit language call = do
  let ignored = U.apply (U.lam U.unit) call
      body = case language of
        PlutusV1 -> U.lam (U.lam (U.lam ignored))
        PlutusV2 -> U.lam (U.lam (U.lam ignored))
        _ -> U.lam ignored
  script <- plutusScript language (U.scriptHex body)
  let hash = hashScript script
      datum = Data (PV1.I 0) :: Data E
      out = mkBasicTxOut (Addr Testnet (ScriptHashObj hash) StakeRefNull) (inject (Coin 5_000_000)) & datumTxOutL .~ DatumHash (hashData datum)
  made <- submitTx (mkBasicTx (mkBasicTxBody & outputsTxBodyL .~ [out]) :: Tx TopTx E)
  collateral <- makeCollateralInput
  submit $
    mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [txInAt 0 made] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 700_000)
      & witsTxL . scriptTxWitsL .~ [(hash, script)]
      & witsTxL . datsTxWitsL .~ TxDats [(hashData datum, datum)]
      & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (Data (PV1.I 0), ExUnits 1_000_000 3_000_000_000))]

-- | Submits a transaction and fails unless the ledger refuses it because a script failed.
refusedByScript :: Tx TopTx E -> ImpTestM E ()
refusedByScript tx = do
  refusal <- expectLeft =<< trySubmitTx tx
  when (not ("FailedUnexpectedly" `isInfixOf` show refusal)) $ assertFailure ("refused for another reason: " <> show refusal)

-- | Submits a transaction and fails unless the ledger accepts it or refuses it because a script failed.
acceptedOrRefusedByScript :: Tx TopTx E -> ImpTestM E ()
acceptedOrRefusedByScript tx = do
  result <- trySubmitTx tx
  case result of
    Right _ -> pure ()
    Left refusal -> when (not ("FailedUnexpectedly" `isInfixOf` show refusal)) $ assertFailure ("refused for another reason: " <> show refusal)

-- | The cost models of the registration parameters, by language index.
registeredCostModels :: IO (Map.Map Word8 [Int64])
registeredCostModels = do
  parameters <- either fail pure =<< A.eitherDecodeFileStrict "/probe/registration-epoch64-parameters.json"
  either fail pure . flip parseEither parameters . A.withObject "parameters" $ \o -> do
    raw <- o A..: "cost_models_raw"
    Map.fromList <$> mapM (\(index, name) -> (,) index <$> raw A..: name) [(0, "PlutusV1"), (1, "PlutusV2"), (2, "PlutusV3"), (3, "PlutusV4")]

main :: IO ()
main =
  withMutants "supp" 1000000 $ \mutants ->
    hspec $
      withImpInit @(LedgerSpec E) $
        capturingKeys $
          modifyImpInitProtVer @E (natVersion @12) $
            modifyImpInitPostSubmitTxHook (recordTo mutants "/probe/out/supp.jsonl") supplement
