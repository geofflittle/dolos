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
import Cardano.Ledger.Address (Addr (..), BootstrapAddress (..), Withdrawals (..), bootstrapKeyHash)
import Cardano.Ledger.Alonzo.Scripts (AsIx (..), PlutusBinary (..), mkBinaryPlutusScript, pattern SpendingPurpose)
import Cardano.Ledger.Alonzo.TxAuxData (AlonzoEraTxAuxData (..))
import Cardano.Ledger.Alonzo.TxWits (Redeemers (..), TxDats (..))
import Cardano.Ledger.BaseTypes (Globals (..), Network (..), StrictMaybe (..), epochInfoPure, mkCertIxPartial, mkTxIxPartial, natVersion, strictMaybeToMaybe)
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Conway.Governance (Constitution (..), ConwayEraGov (..), GovAction (..), constitutionGuardrailsScriptHashL)
import Cardano.Ledger.Conway.TxCert (pattern RegDepositTxCert)
import Cardano.Ledger.Credential (Credential (..), Ptr (..), SlotNo32 (..), StakeReference (..))
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Dijkstra.PParams (ppuPerasCertBoostL)
import Cardano.Ledger.Dijkstra.Scripts (pattern RequireGuard)
import Cardano.Ledger.Hashes (EraIndependentTxBody, SafeHash, extractHash, hashAnnotated)
import Cardano.Ledger.Keys (KeyHash (..), coerceKeyRole)
import Cardano.Ledger.Keys.Bootstrap (BootstrapWitness (..), ChainCode (..), bootstrapWitKeyHash, makeBootstrapWitness)
import Cardano.Ledger.Mary.Value (AssetName (..), MaryValue (..), PolicyID (..), multiAssetFromList)
import Cardano.Ledger.Metadata (Metadatum (..))
import Cardano.Ledger.Plutus.CostModels (mkCostModelsLenient)
import Cardano.Ledger.Plutus.Data (Data (..), Datum (..), dataToBinaryData, hashData)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (Language (..))
import Cardano.Ledger.Plutus.TxInfo (transAddr, transCoinToLovelace, transCred, transKeyHash, transScriptHash)
import Cardano.Ledger.Shelley.LedgerState (newEpochStateGovStateL)
import Cardano.Ledger.Shelley.Scripts (pattern RequireAllOf, pattern RequireSignature)
import Cardano.Ledger.Tools (setMinCoinTxOut)
import Cardano.Ledger.TxIn (mkTxInPartial)
import Cardano.Ledger.Val (inject)
import Cardano.Slotting.EpochInfo (EpochInfo (..), epochInfoEpoch, epochInfoSize, epochInfoSlotLength)
import Cardano.Slotting.Slot (EpochSize (..), SlotNo (..))
import Cardano.Slotting.Time (SystemStart (..), slotLengthToMillisec)
import Codec.CBOR.Term (Term (..))
import Control.Monad (forM_, replicateM_, void, when, (<=<))
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
import Dump (E, dumpTo)
import Edit
import Lens.Micro ((%~), (&), (.~), (^.))
import Lens.Micro.Mtl (use, (%=))
import qualified PlutusLedgerApi.V1 as PV1
import qualified PlutusLedgerApi.V3 as PV3
import Test.Cardano.Ledger.Core.KeyPair (ByronKeyPair (..))
import Test.Cardano.Ledger.Core.Utils (txInAt)
import Test.Cardano.Ledger.Dijkstra.ImpTest
import Test.Cardano.Ledger.Imp.Common

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
spendInRange submit interval range = spendWithV3In submit interval (Data (PV1.toData range)) rangeIsRedeemer

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
  submitEdited noEdit {editSubBody = f} (mkTopTxWithSubTxs [sub] & bodyTxL %~ topBody)

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
customBootstrapSpend level chainCode attributes = do
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
    Top -> withPostFixup signed (submitTx_ (spend :: Tx TopTx E))
    Sub -> withPostFixupSubTxs signed (submitTx_ (mkTopTxWithSubTxs [spend]))

-- | Locks coin at a V3 script given as its CBOR hex, then spends it with that script and no datum.
spendWithV3 :: ByteString -> ImpTestM E ()
spendWithV3 = spendWithV3Using submitTx_

-- | Locks coin at a V3 script given as its CBOR hex, then submits a spend of it with that script and no datum.
spendWithV3Using :: (Tx TopTx E -> ImpTestM E ()) -> ByteString -> ImpTestM E ()
spendWithV3Using submit = spendWithV3In submit (ValidityInterval SNothing SNothing) (Data (PV1.I 0))

-- | Locks coin at a V3 script given as its CBOR hex, then submits a spend of it with that script, the validity interval, the redeemer and no datum.
spendWithV3In :: (Tx TopTx E -> ImpTestM E ()) -> ValidityInterval -> Data E -> ByteString -> ImpTestM E ()
spendWithV3In submit interval redeemer hex = do
  plutus :: PlutusScript E <- expectJust $ mkBinaryPlutusScript PlutusV3 . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode hex)
  let script = fromPlutusScript plutus
      hash = hashScript script
  locked <- sendCoinTo (Addr Testnet (ScriptHashObj hash) StakeRefNull) (Coin 5_000_000)
  collateral <- makeCollateralInput
  let tx :: Tx TopTx E
      tx =
        mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [locked] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 400_000 & vldtTxBodyL .~ interval)
          & witsTxL . scriptTxWitsL .~ [(hash, script)]
          & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (redeemer, ExUnits 1_000_000 1_000_000_000))]
  submit tx

main :: IO ()
main =
  hspec $
    withImpInit @(LedgerSpec E) $
      modifyImpInitProtVer @E (natVersion @12) $
        modifyImpInitPostSubmitTxHook (dumpTo "/probe/out/supp.jsonl") supplement
