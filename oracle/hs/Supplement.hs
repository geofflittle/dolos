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
import Cardano.Ledger.Alonzo.TxWits (Redeemers (..), TxDats (..))
import Cardano.Ledger.BaseTypes (Network (..), StrictMaybe (..), natVersion)
import Cardano.Ledger.Coin (Coin (..))
import Cardano.Ledger.Credential (Credential (..), StakeReference (..))
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Hashes (EraIndependentTxBody, SafeHash, extractHash, hashAnnotated)
import Cardano.Ledger.Keys (KeyHash (..), coerceKeyRole)
import Cardano.Ledger.Keys.Bootstrap (BootstrapWitness (..), ChainCode (..), bootstrapWitKeyHash, makeBootstrapWitness)
import Cardano.Ledger.Mary.Value (AssetName (..), MaryValue (..), PolicyID (..), multiAssetFromList)
import Cardano.Ledger.Metadata (Metadatum (..))
import Cardano.Ledger.Plutus.Data (Data (..), Datum (..), dataToBinaryData, hashData)
import Cardano.Ledger.Plutus.ExUnits (ExUnits (..))
import Cardano.Ledger.Plutus.Language (Language (..))
import Cardano.Ledger.Shelley.Scripts (pattern RequireSignature)
import Cardano.Ledger.Tools (setMinCoinTxOut)
import Cardano.Ledger.TxIn (mkTxInPartial)
import Cardano.Ledger.Val (inject)
import Codec.CBOR.Term (Term (..))
import Control.Monad (forM_, replicateM_, void)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Lazy as BL
import Data.ByteString.Short (toShort)
import Data.Maybe (listToMaybe)
import qualified Data.OMap.Strict as OMap
import Data.Primitive.ByteArray (byteArrayFromList)
import Data.Typeable (Typeable)
import Data.Word (Word8)
import Dump (E, dumpTo)
import Edit
import Lens.Micro ((%~), (&), (.~), (^.))
import qualified PlutusLedgerApi.V1 as PV1
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
spendWithV3Using submit hex = do
  plutus :: PlutusScript E <- expectJust $ mkBinaryPlutusScript PlutusV3 . PlutusBinary . toShort =<< either (const Nothing) Just (B16.decode hex)
  let script = fromPlutusScript plutus
      hash = hashScript script
  locked <- sendCoinTo (Addr Testnet (ScriptHashObj hash) StakeRefNull) (Coin 5_000_000)
  collateral <- makeCollateralInput
  let tx :: Tx TopTx E
      tx =
        mkBasicTx (mkBasicTxBody & inputsTxBodyL .~ [locked] & collateralInputsTxBodyL .~ [collateral] & feeTxBodyL .~ Coin 400_000)
          & witsTxL . scriptTxWitsL .~ [(hash, script)]
          & witsTxL . rdmrsTxWitsL .~ Redeemers [(SpendingPurpose (AsIx 0), (Data (PV1.I 0), ExUnits 1_000_000 1_000_000_000))]
  submit tx

main :: IO ()
main =
  hspec $
    withImpInit @(LedgerSpec E) $
      modifyImpInitProtVer @E (natVersion @12) $
        modifyImpInitPostSubmitTxHook (dumpTo "/probe/out/supp.jsonl") supplement
