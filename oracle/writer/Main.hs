{-# LANGUAGE CPP #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RecordWildCards #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}

-- | Writes the reference records of the ledger commit it is built at: generated Dijkstra values with the ledger's reading of each.
module Main (main) where

import qualified Build
import qualified Mutation
import qualified SignedChange
import Cardano.Ledger.Address (DirectDeposits (..))
import Cardano.Ledger.Allegra.Scripts (Timelock, TimelockRaw (..), ValidityInterval (..))
import Cardano.Ledger.Alonzo.Scripts (AlonzoEraScript, AlonzoScript (..), plutusScriptBinary, plutusScriptLanguage)
import Cardano.Ledger.Alonzo.Tx (AlonzoEraTx (..), IsPhase2Valid (..))
import Cardano.Ledger.Alonzo.TxAuxData (AlonzoTxAuxDataRaw (..))
import Cardano.Ledger.Alonzo.TxWits (AlonzoEraTxWits (..), TxDats (..), unRedeemers, unTxDats)
import Cardano.Ledger.Babbage.TxOut (BabbageEraTxOut (..))
import Cardano.Ledger.BaseTypes (StrictMaybe (..))
import Cardano.Ledger.Binary (
  Annotator,
  DecCBOR (..),
  EncCBOR,
  Sized (..),
  Version,
  decodeFull',
  decodeFullAnnotator,
  encodeFixedSized,
  serialize',
 )
import Cardano.Ledger.Block (Block (..))
import Cardano.Ledger.Conway (ConwayEra)
import Cardano.Ledger.Conway.Governance (GovAction (..), ProposalProcedure (..), VotingProcedures (..))
import Cardano.Ledger.Core
import Cardano.Ledger.Dijkstra (DijkstraEra)
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
import Cardano.Ledger.Dijkstra.Block.Endorser (EbReferences, getEbReferences)
#endif
import Cardano.Ledger.Dijkstra.BlockBody (DijkstraEraBlockBody (..))
import Cardano.Ledger.Dijkstra.HuddleSpec (dijkstraCDDL)
import Cardano.Ledger.Dijkstra.Scripts (AccountBalanceIntervals (..), DijkstraNativeScriptRaw (..))
import Cardano.Ledger.Dijkstra.TxBody (DijkstraEraTxBody (..), DijkstraTxBodyRaw (..))
import Cardano.Ledger.Dijkstra.TxCert (DijkstraTxCert (..))
import Cardano.Ledger.Mary.Value (MaryValue (..), MultiAsset (..))
import Cardano.Ledger.MemoBytes (getMemoRawType)
import Cardano.Crypto.Hash (hashToBytes)
import Cardano.Ledger.Plutus.Data (Data, Datum (..), binaryDataToData, dataToBinaryData, getPlutusData, hashBinaryData, hashData)
import qualified Data.Sequence.Strict as StrictSeq
import Cardano.Ledger.Plutus.Language (Language (..), PlutusBinary (..))
import Cardano.Ledger.State (StakePoolParams (..), UTxO (..))
import Cardano.Ledger.TxIn (TxIn)
import Cardano.Protocol.Crypto (StandardCrypto)
import qualified Cardano.Protocol.Leios.BlockHeader as Leios
import Codec.CBOR.Cuddle.CBOR.Gen (generateFromName)
import Codec.CBOR.Cuddle.CBOR.Validator (validateCBOR)
import Codec.CBOR.Cuddle.CBOR.Validator.Trace (
  ControlInfo (..),
  Evidenced (..),
  ListValidationTrace (..),
  MapValidationTrace (..),
  SValidity (..),
  ValidationTrace (..),
 )
import Codec.CBOR.Cuddle.CDDL (Name (..))
import Codec.CBOR.Cuddle.CDDL.CTree (CTree (..), CTreeRoot (..), foldCTree)
import Codec.CBOR.Cuddle.CDDL.CtlOp (CtlOp (Cbor))
import Codec.CBOR.Cuddle.CDDL.Custom.Generator (runCBORGen)
import Codec.CBOR.Cuddle.CDDL.Custom.Validator (ValidatorPhase, XXCTree (..))
import Codec.CBOR.Cuddle.CDDL.Resolve (MonoReferenced, XXCTree (..))
import Codec.CBOR.Cuddle.IndexMappable (mapIndex)
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Term as CBOR
import qualified Codec.CBOR.Write as CBOR
import Data.Aeson ((.=))
import qualified Data.Aeson as Aeson
import qualified Data.Aeson.Encoding as Encoding
import qualified Data.Aeson.Key as Key
import qualified Data.Aeson.Types as Aeson (parseMaybe)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Char8 as BS8
import qualified Data.ByteString.Lazy as BSL
import qualified Data.ByteString.Lazy.Char8 as BSL8
import qualified Data.ByteString.Short as SBS
import Control.Monad (unless)
import Data.Containers.ListUtils (nubOrd)
import Data.Either (isRight)
import Data.Foldable (toList)
import Data.List (foldl', sortOn)
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import qualified Data.OMap.Strict as OMap
import qualified Data.OSet.Strict as OSet
import qualified Data.Set as Set
#if MIN_VERSION_cardano_data(1,4,0)
import qualified Data.SimpleOMap.Strict as SimpleOMap
#endif
import qualified Data.Text as T
import qualified Data.Text.Encoding as T
import Lens.Micro ((&), (.~), (^.))
import qualified PlutusLedgerApi.V1 as PV1
import System.Environment (getArgs)
import Test.AntiGen (ZapResult (..), runAntiGen, zapAntiGenResult)
import Test.Cardano.Ledger.Binary.Cuddle (HuddleEnv (..), resolveHuddle, toGenConfig)
import Test.Cardano.Ledger.Dijkstra.Arbitrary (
  genSmallDijkstraCertBlockBody,
  genSmallDijkstraTxsBlockBody,
 )
import Test.Cardano.Protocol.Leios.BlockHeader.Arbitrary ()
import Test.QuickCheck (Gen, arbitrary, vectorOf)
import Test.QuickCheck.Gen (unGen)
import Test.QuickCheck.Random (mkQCGen)

type LeiosBlock = Block (Leios.Header StandardCrypto) DijkstraEra

-- | The kinds of value the writer generates, the endorser block references only at a ledger commit that defines them.
data Kind
  = KTx
  | KBody
  | KWits
  | KPpu
  | KBlock
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
  | KEb
#endif

ruleOf :: Kind -> T.Text
ruleOf = \case
  KTx -> "mempool_transaction"
  KBody -> "transaction_body"
  KWits -> "transaction_witness_set"
  KPpu -> "protocol_param_update"
  KBlock -> "block"
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
  KEb -> "eb_references"
#endif

ver :: Version
ver = eraProtVerHigh @DijkstraEra

hexT :: BS.ByteString -> T.Text
hexT = T.decodeUtf8 . B16.encode

-- Generation ------------------------------------------------------------------

arbBytes :: Kind -> Int -> Gen BS.ByteString
arbBytes k i = case k of
  KTx -> serialize' ver <$> arbitrary @(Tx TopTx DijkstraEra)
  KBody -> serialize' ver <$> arbitrary @(TxBody TopTx DijkstraEra)
  KWits -> serialize' ver <$> arbitrary @(TxWits DijkstraEra)
  KPpu -> serialize' ver <$> arbitrary @(PParamsUpdate DijkstraEra)
  KBlock -> do
    h <- arbitrary @(Leios.Header StandardCrypto)
    b <- if even i then genSmallDijkstraTxsBlockBody else genSmallDijkstraCertBlockBody
    pure $ serialize' ver (Block h b :: LeiosBlock)
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
  KEb -> serialize' ver <$> arbitrary @EbReferences
#endif

cddlBytes :: HuddleEnv -> Kind -> Gen BS.ByteString
cddlBytes env k = do
  term <- runAntiGen . runCBORGen (toGenConfig env) . generateFromName $ Name (ruleOf k)
  pure $ CBOR.toStrictByteString (CBOR.encodeTerm term)

-- Rule credit -----------------------------------------------------------------

-- | The names of the rules a validation trace matched.
traceRules :: ValidationTrace v -> [Name]
traceRules = \case
  ReferenceRule n t -> n : traceRules t
  ControlTrace (ControlInfo Cbor (CTreeE (MSimpleRef n))) t -> n : traceRules t
  ControlTrace _ t -> traceRules t
  ChoiceBranch _ t -> traceRules t
  ListTrace t -> listRules t
  MapTrace t -> mapRules t
  TagTrace _ t -> traceRules t
  _ -> []

listRules :: ListValidationTrace v -> [Name]
listRules = \case
  ListValidationConsume _ t rest -> traceRules t <> listRules rest
  ListValidationConsumeGroup ns t rest -> ns <> listRules t <> listRules rest
  _ -> []

mapRules :: MapValidationTrace v -> [Name]
mapRules = \case
  MapValidationConsume _ k v rest -> traceRules k <> traceRules v <> mapRules rest
  _ -> []

-- | The spec as plain CDDL, each rule with a custom validator replaced by the rule the validator refines.
plainRoot :: CTreeRoot MonoReferenced -> CTreeRoot ValidatorPhase
plainRoot (CTreeRoot m) = CTreeRoot (plainTree <$> m)

plainTree :: CTree MonoReferenced -> CTree ValidatorPhase
plainTree = foldCTree ext plainTree
  where
    ext (MRuleRef n) = CTreeE (VRuleRef n)
    ext (MGenerator _ x) = plainTree x
    ext (MValidator _ x) = plainTree x

-- | The rule of the CDDL text a rule of the spec instantiates, `set` for `%set<transaction_input>`.
textRule :: Name -> T.Text
textRule = T.takeWhile (/= '<') . T.dropWhile (== '%') . unName

-- | The integers of a term, each tagged bignum among them.
integers :: CBOR.Term -> [Integer]
integers = \case
  CBOR.TInteger i -> [i]
  CBOR.TList ts -> concatMap integers ts
  CBOR.TListI ts -> concatMap integers ts
  CBOR.TMap kvs -> concatMap (\(k, v) -> integers k <> integers v) kvs
  CBOR.TMapI kvs -> concatMap (\(k, v) -> integers k <> integers v) kvs
  CBOR.TTagged _ t -> integers t
  _ -> []

-- | The rules `big_uint` and `big_nint` for each integer of the value above or below the range of `int`.
bigRules :: BS.ByteString -> [T.Text]
bigRules bs = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict bs) of
  Right (_, t) ->
    let is = integers t
     in ["big_uint" | any (> 2 ^ (64 :: Int) - 1) is] <> ["big_nint" | any (< negate (2 ^ (64 :: Int))) is]
  Left _ -> []

-- | The sorted names of the rules the value matches when the plain spec accepts it, and none otherwise.
rulesOf :: CTreeRoot ValidatorPhase -> Kind -> BS.ByteString -> [T.Text]
rulesOf root k bs = case validateCBOR bs (Name (ruleOf k)) root of
  Right (Evidenced SValid t) ->
    -- The validator credits `big_uint` for a tagged bignum of either sign.
    let traced = filter (/= "big_uint") (map textRule (traceRules t))
     in Set.toList (Set.fromList (ruleOf k : traced <> bigRules bs))
  _ -> []

-- Ledger reading ----------------------------------------------------------------

-- | A decoded value with each leaf the ledger's own encoding of it.
data D
  = DS T.Text
  | DC BS.ByteString
  | DNull
  | DB Bool
  | DL [D]
  | DSet [D]
  | DOSet [D]
  | DMap [(D, D)]
  | DOMap [(D, D)]
  | DO [(T.Text, D)]

toJ :: D -> Aeson.Value
toJ = \case
  DS t -> Aeson.String t
  DC bs -> Aeson.object ["cbor" .= hexT bs]
  DNull -> Aeson.Null
  DB b -> Aeson.Bool b
  DL xs -> Aeson.toJSON (map toJ xs)
  DSet xs -> Aeson.object ["set" .= map toJ xs]
  DOSet xs -> Aeson.object ["oset" .= map toJ xs]
  DMap kvs -> Aeson.object ["map" .= [[toJ k, toJ v] | (k, v) <- kvs]]
  DOMap kvs -> Aeson.object ["omap" .= [[toJ k, toJ v] | (k, v) <- kvs]]
  DO fs -> Aeson.object [Key.fromText k .= toJ v | (k, v) <- fs]

c :: EncCBOR a => a -> D
c = DC . serialize' ver

int :: Integral a => a -> D
int = DS . T.pack . show . toInteger

tagged :: T.Text -> [(T.Text, D)] -> D
tagged t fs = DO (("tag", DS t) : fs)

opt :: (a -> D) -> StrictMaybe a -> D
opt _ SNothing = DNull
opt f (SJust x) = f x

mapD :: (k -> D) -> (v -> D) -> Map.Map k v -> D
mapD f g m = DMap [(f k, g v) | (k, v) <- Map.toList m]

setD :: (a -> D) -> Set.Set a -> D
setD f = DSet . map f . Set.toList

osetD :: (a -> D) -> OSet.OSet a -> D
osetD f = DOSet . map f . toList

multiD :: MultiAsset -> D
multiD (MultiAsset m) = mapD c (mapD c c) m

plutusD :: PV1.Data -> D
plutusD = \case
  PV1.Constr i fs -> tagged "constr" [("ix", int i), ("fields", DL (map plutusD fs))]
  PV1.Map kvs -> tagged "map" [("entries", DL [DL [plutusD k, plutusD v] | (k, v) <- kvs])]
  PV1.List xs -> tagged "list" [("items", DL (map plutusD xs))]
  PV1.I n -> tagged "int" [("v", int n)]
  PV1.B b -> tagged "bytes" [("v", DS (hexT b))]

dataD :: Era era => Data era -> D
dataD = plutusD . getPlutusData

langName :: Language -> T.Text
langName = \case
  PlutusV1 -> "plutus_v1"
  PlutusV2 -> "plutus_v2"
  PlutusV3 -> "plutus_v3"
  PlutusV4 -> "plutus_v4"

binD :: PlutusBinary -> D
binD = DS . hexT . SBS.fromShort . unPlutusBinary

nativeD :: NativeScript DijkstraEra -> D
nativeD ns = case getMemoRawType ns of
  DijkstraRequireSignature h -> tagged "sig" [("hash", c h)]
  DijkstraRequireAllOf xs -> tagged "all" [("scripts", DL (map nativeD (toList xs)))]
  DijkstraRequireAnyOf xs -> tagged "any" [("scripts", DL (map nativeD (toList xs)))]
  DijkstraRequireMOf m xs -> tagged "m_of" [("m", int m), ("scripts", DL (map nativeD (toList xs)))]
  DijkstraTimeStart s -> tagged "start" [("slot", c s)]
  DijkstraTimeExpire s -> tagged "expire" [("slot", c s)]
  DijkstraRequireGuard cr -> tagged "guard" [("cred", c cr)]

timelockD :: Timelock ConwayEra -> D
timelockD ns = case getMemoRawType ns of
  TimelockSignature h -> tagged "sig" [("hash", c h)]
  TimelockAllOf xs -> tagged "all" [("scripts", DL (map timelockD (toList xs)))]
  TimelockAnyOf xs -> tagged "any" [("scripts", DL (map timelockD (toList xs)))]
  TimelockMOf m xs -> tagged "m_of" [("m", int m), ("scripts", DL (map timelockD (toList xs)))]
  TimelockTimeStart s -> tagged "start" [("slot", c s)]
  TimelockTimeExpire s -> tagged "expire" [("slot", c s)]

scriptD :: (Script era ~ AlonzoScript era, AlonzoEraScript era) => (NativeScript era -> D) -> Script era -> D
scriptD native = \case
  NativeScript ns -> tagged "native" [("script", native ns)]
  PlutusScript ps -> tagged (langName (plutusScriptLanguage ps)) [("bytes", binD (plutusScriptBinary ps))]

outD :: (BabbageEraTxOut era, Value era ~ MaryValue, Script era ~ AlonzoScript era, AlonzoEraScript era) => (NativeScript era -> D) -> TxOut era -> D
outD native o =
  let MaryValue coin ma = o ^. valueTxOutL
   in DO
        [ ("address", c (o ^. addrTxOutL))
        , ("coin", c coin)
        , ("assets", multiD ma)
        , ( "datum"
          , case o ^. datumTxOutL of
              NoDatum -> DNull
              DatumHash h -> tagged "hash" [("hash", c h)]
              Datum bd -> tagged "inline" [("data", dataD (binaryDataToData bd))]
          )
        , ("script", opt (scriptD native) (o ^. referenceScriptTxOutL))
        ]

certD :: TxCert DijkstraEra -> D
certD = \case
  DijkstraTxCertPool (RegPool p) ->
    tagged
      "pool_reg"
      [ ("operator", c (sppId p))
      , ("vrf", c (sppVrf p))
      , ("bls", opt c (sppBlsKey p))
      , ("pledge", c (sppPledge p))
      , ("cost", c (sppCost p))
      , ("margin", c (sppMargin p))
      , ("account", c (sppAccountAddress p))
      , ("owners", setD c (sppOwners p))
      , ("relays", DL (map c (toList (sppRelays p))))
      , ("metadata", opt c (sppMetadata p))
      ]
  cert -> c cert

actionD :: GovAction DijkstraEra -> D
actionD = \case
  TreasuryWithdrawals ws pol ->
    tagged "treasury_withdrawals" [("withdrawals", mapD c c ws), ("policy", opt c pol)]
  UpdateCommittee prev rm add thr ->
    tagged
      "update_committee"
      [ ("prev", opt c prev)
      , ("remove", setD c rm)
      , ("add", mapD c c add)
      , ("threshold", c thr)
      ]
  a -> c a

proposalD :: ProposalProcedure DijkstraEra -> D
proposalD p =
  DO
    [ ("deposit", c (pProcDeposit p))
    , ("account", c (pProcReturnAddr p))
    , ("action", actionD (pProcGovAction p))
    , ("anchor", c (pProcAnchor p))
    ]

votesD :: VotingProcedures DijkstraEra -> D
votesD (VotingProcedures m) = mapD c (mapD c c) m

intervalsD :: AccountBalanceIntervals DijkstraEra -> D
intervalsD (AccountBalanceIntervals m) = mapD c c m

-- | The fields a top level body and a sub transaction body both hold.
sharedD ::
  Set.Set TxIn ->
  Set.Set TxIn ->
  [Sized (TxOut DijkstraEra)] ->
  OSet.OSet (TxCert DijkstraEra) ->
  Withdrawals ->
  ValidityInterval ->
  [(T.Text, D)]
sharedD ins refs outs certs (Withdrawals ws) vldt =
  [ ("inputs", setD c ins)
  , ("reference_inputs", setD c refs)
  , ("outputs", DL (map (outD nativeD . sizedValue) outs))
  , ("certs", osetD certD certs)
  , ("withdrawals", mapD c c ws)
  , ("ttl", opt c (invalidHereafter vldt))
  , ("start", opt c (invalidBefore vldt))
  ]

bodyD :: TxBody l DijkstraEra -> D
bodyD body = case getMemoRawType body of
  DijkstraTxBodyRaw {..} ->
    DO $
      sharedD dtbrSpendInputs dtbrReferenceInputs (toList dtbrOutputs) dtbrCerts dtbrWithdrawals dtbrVldt
        <> [ ("fee", c dtbrFee)
           , ("collateral", setD c dtbrCollateralInputs)
           , ("collateral_return", opt (outD nativeD . sizedValue) dtbrCollateralReturn)
           , ("total_collateral", opt c dtbrTotalCollateral)
           , ("guards", osetD c dtbrGuards)
           , ("mint", multiD dtbrMint)
           , ("script_data_hash", opt c dtbrScriptIntegrityHash)
           , ("aux_hash", opt c dtbrAuxDataHash)
           , ("network", opt c dtbrNetworkId)
           , ("votes", votesD dtbrVotingProcedures)
           , ("proposals", osetD proposalD dtbrProposalProcedures)
           , ("treasury", opt c dtbrCurrentTreasuryValue)
           , ("donation", c dtbrTreasuryDonation)
           , ("sub_txs", DOMap [(c k, subTxD v) | (k, v) <- OMap.assocList dtbrSubTransactions])
           , ("top_guards", mapD c (opt dataD) dtbrRequiredTopLevelGuards)
           , ("deposits", mapD c c (unDirectDeposits dtbrDirectDeposits))
           , ("balance_intervals", intervalsD dtbrAccountBalanceIntervals)
           , ("starting_balance_intervals", intervalsD dtbrStartingAccountBalanceIntervals)
           ]
  DijkstraSubTxBodyRaw {..} ->
    DO $
      sharedD dstbrSpendInputs dstbrReferenceInputs (toList dstbrOutputs) dstbrCerts dstbrWithdrawals dstbrVldt
        <> [ ("guards", osetD c dstbrGuards)
           , ("mint", multiD dstbrMint)
           , ("script_data_hash", opt c dstbrScriptIntegrityHash)
           , ("aux_hash", opt c dstbrAuxDataHash)
           , ("network", opt c dstbrNetworkId)
           , ("votes", votesD dstbrVotingProcedures)
           , ("proposals", osetD proposalD dstbrProposalProcedures)
           , ("treasury", opt c dstbrCurrentTreasuryValue)
           , ("donation", c dstbrTreasuryDonation)
           , ("top_guards", mapD c (opt dataD) dstbrRequiredTopLevelGuards)
           , ("deposits", mapD c c (unDirectDeposits dstbrDirectDeposits))
           , ("balance_intervals", intervalsD dstbrAccountBalanceIntervals)
           ]

-- | A top level body with the transaction id it gives.
topBodyD :: TxBody TopTx DijkstraEra -> D
topBodyD body = case bodyD body of
  DO fs -> DO (("id", c (txIdTxBody body)) : fs)
  d -> d

witsD :: TxWits DijkstraEra -> D
witsD w =
  DO
    [ ("vkeys", setD c (w ^. addrTxWitsL))
    , ("boots", setD c (w ^. bootAddrTxWitsL))
    , ("scripts", mapD c (scriptD nativeD) (w ^. scriptTxWitsL))
    , ("datums", mapD c dataD (unTxDats (w ^. datsTxWitsL)))
    , ("redeemers", mapD c (\(d, u) -> DO [("data", dataD d), ("units", c u)]) (unRedeemers (w ^. rdmrsTxWitsL)))
    ]

auxD :: TxAuxData DijkstraEra -> D
auxD a = case getMemoRawType a of
  AlonzoTxAuxDataRaw md ns ps ->
    DO
      [ ("metadata", mapD c c md)
      , ("native", DL (map nativeD (toList ns)))
      , ("plutus", mapD (DS . langName) (DL . map binD . NE.toList) ps)
      ]

subTxD :: Tx SubTx DijkstraEra -> D
subTxD tx =
  DO
    [ ("body", bodyD (tx ^. bodyTxL))
    , ("wits", witsD (tx ^. witsTxL))
    , ("aux", opt auxD (tx ^. auxDataTxL))
    ]

txD :: Tx TopTx DijkstraEra -> D
txD tx =
  DO
    [ ("id", c (txIdTx tx))
    , ("body", bodyD (tx ^. bodyTxL))
    , ("wits", witsD (tx ^. witsTxL))
    , ("valid", DB (case tx ^. isPhase2ValidTxL of Phase2Valid -> True; Phase2Invalid -> False))
    , ("aux", opt auxD (tx ^. auxDataTxL))
    ]

blockD :: LeiosBlock -> D
blockD (Block h b) =
  let hb = Leios.headerBody h
   in DO
        [ ( "header"
          , DO
              [ ("block_no", c (Leios.hbBlockNo hb))
              , ("slot", c (Leios.hbSlotNo hb))
              , ("prev", c (Leios.hbPrev hb))
              , ("issuer_vk", c (Leios.hbVk hb))
              , ("vrf_vk", c (Leios.hbVrfVk hb))
              , ("vrf", c (Leios.hbVrfRes hb))
              , ("body_size", c (Leios.hbBodySize hb))
              , ("body_hash", c (Leios.hbBodyHash hb))
              , ("ocert", c (Leios.hbOCert hb))
#if MIN_VERSION_cardano_protocol(0,3,0)
              , ("version", c (Leios.hbVersionInfo hb))
#else
              , ("version", c (Leios.hbProtVer hb))
#endif
              , ("leios_cert", DB (Leios.hbBlockBodyContainsLeiosCert hb))
#if MIN_VERSION_cardano_protocol(0,3,0)
              , ("eb", opt c (Leios.hbEbReferencesAnnouncement hb))
#else
              , ("eb", opt c (Leios.hbEbAnnouncement hb))
#endif
              , ("sig", c (encodeFixedSized (Leios.headerSig h)))
              , ("hash", c (Leios.headerHash h))
              ]
          )
        , ("txs", DL (map txD (toList (b ^. txSeqBlockBodyL))))
        , ("leios_cert", opt c (b ^. leiosCertBlockBodyL))
        , ("peras_cert", opt c (b ^. perasCertBlockBodyL))
        ]

#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
ebD :: EbReferences -> D
ebD x = DOMap [(c h, c n) | (h, n) <- SimpleOMap.assocList (getEbReferences x)]
#endif

-- | The ledger's refusal of a value, as the records write it.
refusal :: Show e => e -> T.Text
refusal = T.pack . take 2000 . show

-- | Decodes with the plain decoder of the type.
plain :: forall a. DecCBOR a => (a -> D) -> BS.ByteString -> Either T.Text D
plain f bs = either (Left . refusal) (Right . f) (decodeFull' @a ver bs)

-- | Decodes with the annotator instance the node itself uses for the type.
annotated :: forall a. DecCBOR (Annotator a) => (a -> D) -> BS.ByteString -> Either T.Text D
annotated f bs =
  either (Left . refusal) (Right . f) (decodeFullAnnotator @a ver "oracle" decCBOR (BSL.fromStrict bs))

-- | The ledger's reading of a value of a kind, or its reason to refuse it.
reading :: Kind -> BS.ByteString -> Either T.Text D
reading = \case
  KTx -> annotated @(Tx TopTx DijkstraEra) txD
  KBody -> annotated @(TxBody TopTx DijkstraEra) topBodyD
  KWits -> annotated @(TxWits DijkstraEra) witsD
  KPpu -> plain @(PParamsUpdate DijkstraEra) c
  KBlock -> annotated @LeiosBlock blockD
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
  KEb -> plain @EbReferences ebD
#endif

-- Records ---------------------------------------------------------------------

-- | Prints one JSON object with its fields in the order given.
line :: Aeson.Series -> IO ()
line = BSL8.putStrLn . Encoding.encodingToLazyByteString . Aeson.pairs

-- | Prints the header of a record file, naming the ledger commit and this writer.
header :: T.Text -> [T.Text] -> Aeson.Series -> IO ()
header layer classes extra =
  line $
    "reference" .= T.pack Build.pin
      <> "generator" .= T.pack Build.name
      <> extra
      <> "layer" .= layer
      <> "classes" .= classes

-- | The ledger's answer for one input, as a record writes it.
answer :: Either T.Text D -> Aeson.Value
answer = \case
  Right d -> Aeson.object ["accepts" .= toJ d]
  Left reason -> Aeson.object ["refuses" .= reason]

-- | One record of an input with the ledger's answer for it.
record :: T.Text -> T.Text -> BS.ByteString -> Either T.Text D -> Aeson.Series -> Aeson.Series
record class_ variant input reading_ extra =
  "class" .= class_
    <> "variant" .= variant
    <> "input" .= hexT input
    <> "reference" .= answer reading_
    <> extra

-- | The kinds of the decode records, each with its class name.
decodeKinds :: [(T.Text, Kind)]
decodeKinds =
  [ ("tx", KTx)
  , ("body", KBody)
  , ("wits", KWits)
  , ("ppu", KPpu)
  , ("block", KBlock)
#if MIN_VERSION_cardano_ledger_dijkstra(0,5,0)
  , ("eb", KEb)
#endif
  ]

-- | The sources of generated values, each with the number of values drawn from it.
sources :: [(String, Int)]
sources = [("arb", 10), ("cddl", 30), ("cddl-plain", 30)]

-- | The kinds the changed values of the decode records are drawn as.
changedKinds :: [(T.Text, Kind)]
changedKinds = [("body", KBody), ("wits", KWits), ("ppu", KPpu), ("block", KBlock), ("tx", KTx)]

-- | The number of values drawn of each changed kind, and of each zapped kind.
changedDraws, zappedDraws :: Int
changedDraws = 300
zappedDraws = 30

-- | The forms other than the shortest that a whole changed value is written in, each with its kind of change.
wholeForms :: [(T.Text, Mutation.Form)]
wholeForms = [("wide-heads", Mutation.WideHeads), ("indefinite-lengths", Mutation.IndefiniteLengths)]

-- | The record of a value of a class with the ledger's answer for it.
data Out = Out {outClass :: T.Text, outRecord :: Aeson.Series}

-- | The values a fault injected by the generator makes the full spec refuse
-- in a value the ledger reads, each with the rules the generator was in where
-- it injected the fault and the value before the fault.
zapped :: CTreeRoot MonoReferenced -> [Out]
zapped root =
  [ Out class_ (record class_ (T.pack ("zap " <> show i)) bs (reading k bs) ("mutated" .= faults zr <> "base" .= hexT base))
  | (name, k) <- changedKinds
  , i <- [0 .. zappedDraws - 1]
  , let draw n = unGen (zapAntiGenResult n (runCBORGen cfg (generateFromName (Name (ruleOf k))))) (mkQCGen (1 + i)) (1 + i `mod` 10)
        zr = draw 1
        bs = encoded (zrValue zr)
        base = encoded (zrValue (draw 0))
        class_ = name <> " zap"
  , zrZapped zr > 0
  , refusedBySpec (Name (ruleOf k)) bs
  , isRight (reading k base)
  ]
  where
    encoded = CBOR.toStrictByteString . CBOR.encodeTerm
    cfg = toGenConfig HuddleEnv {heTwiddle = False, heRoot = root}
    full = mapIndex root :: CTreeRoot ValidatorPhase
    refusedBySpec n bs = case validateCBOR bs n full of
      Right (Evidenced SInvalid _) -> True
      _ -> False
    faults = T.intercalate " " . map (T.intercalate "/" . NE.toList) . zrAnnotation

-- | The rules the generator draws without marks, since the generator of sub
-- transactions reads the first item of each element and draws the elements
-- itself.
unmarked :: Set.Set Name
unmarked = Set.fromList [subTransaction, subTransactions]

-- | The rule of one sub transaction and the rule of the set of them.
subTransaction, subTransactions :: Name
subTransaction = Name "sub_transaction"
subTransactions = Name "%nonempty_oset<sub_transaction>"

-- | The value drawn of a kind at a seed from a spec.
drawn :: CTreeRoot MonoReferenced -> Kind -> Int -> CBOR.Term
drawn root k i = unGen (runAntiGen (runCBORGen cfg (generateFromName (Name (ruleOf k))))) (mkQCGen (1 + i)) (1 + i `div` 5)
  where
    cfg = toGenConfig HuddleEnv {heTwiddle = False, heRoot = root}

-- | The marked value of a kind at a seed written without its marks, its
-- occurrences, the marked term and the rules marked.
markedDraw :: CTreeRoot MonoReferenced -> Set.Set Name -> Kind -> Int -> (BS.ByteString, [Mutation.Occurrence], CBOR.Term, [(Name, Mutation.Shape)])
markedDraw root excluded k i =
  let (root', table) = Mutation.marked excluded root
      term = markSubs table (drawn root' k i)
      (bs, occs) = Mutation.write table Nothing Mutation.Shortest term
   in (bs, occs, term, table)

-- | A drawn value with the sub transactions of each transaction body under the marks of their rules.
markSubs :: [(Name, Mutation.Shape)] -> CBOR.Term -> CBOR.Term
markSubs table = go
  where
    body = Mutation.markOf table (Name "transaction_body")
    go = \case
      CBOR.TTagged tag (CBOR.TMap kvs) | Just tag == body -> CBOR.TTagged tag (CBOR.TMap (map field kvs))
      CBOR.TTagged tag (CBOR.TMapI kvs) | Just tag == body -> CBOR.TTagged tag (CBOR.TMapI (map field kvs))
      CBOR.TTagged tag x -> CBOR.TTagged tag (go x)
      CBOR.TList xs -> CBOR.TList (map go xs)
      CBOR.TListI xs -> CBOR.TListI (map go xs)
      CBOR.TMap kvs -> CBOR.TMap [(go key, go v) | (key, v) <- kvs]
      CBOR.TMapI kvs -> CBOR.TMapI [(go key, go v) | (key, v) <- kvs]
      x -> x
    field (key, v)
      | key `elem` [CBOR.TInt 23, CBOR.TInteger 23] = (key, Mutation.mark table subTransactions (elements (go v)))
      | otherwise = (key, go v)
    elements = \case
      CBOR.TTagged 258 x -> CBOR.TTagged 258 (elements x)
      CBOR.TList xs -> CBOR.TList (map (Mutation.mark table subTransaction) xs)
      CBOR.TListI xs -> CBOR.TListI (map (Mutation.mark table subTransaction) xs)
      x -> x

-- | The smallest value one change makes to one marked value of a generated
-- value the ledger decodes, for each change at each site of each rule, and
-- the generated values written in each of `wholeForms`, each with the
-- generated value, in the order of their draws.
changed :: CTreeRoot MonoReferenced -> Set.Set Name -> [Out]
changed root excluded = [out | (_, _, out) <- sortOn (\(_, order, _) -> order) (Map.elems (foldl' visit Map.empty (zip [0 :: Int ..] draws)))]
  where
    draws = [(name, k, i) | (name, k) <- changedKinds, i <- [0 .. changedDraws - 1]]
    visit best (n, (name, k, i)) = case markedDraw root excluded k i of
      (base, _, _, _) | Left _ <- reading k base -> best
      (base, occs, term, table) ->
        let candidates =
              [ (form, "", ruleOf k, "", fst (Mutation.write table Nothing f term))
              | (form, f) <- wholeForms
              ]
                <> [ (Mutation.changeKind ch, Mutation.occSite o, unName (Mutation.occRule o), Mutation.changeDetail ch, bs)
                   | o <- occs
                   , ch <- Mutation.changes (Mutation.occShape o) (Mutation.occTerm o)
                   , let bs = fst (Mutation.write table (Just (Mutation.occNumber o, Mutation.changeApply ch)) Mutation.Shortest term)
                   ]
            step acc (j, (kind, site, rule, detail, bs))
              | bs == base = acc
              | otherwise =
                  let class_ = name <> " " <> kind
                      extra = "site" .= site <> "mutated" .= rule <> "base" .= hexT base
                      out = Out class_ (record class_ (T.unwords (T.pack (show i) : [detail | detail /= ""])) bs (reading k bs) extra)
                   in Map.insertWith smaller (kind, site, rule) (BS.length bs + BS.length base, (n, j), out) acc
         in foldl' step best (zip [0 :: Int ..] candidates)
    smaller new@(size, _, _) old@(size', _, _) = if size < size' then new else old

-- | Prints the decode records: of the values each source draws for each kind,
-- from seed 1 at sizes 1 to 10, every one the ledger decodes, then the zapped
-- and the changed values with the ledger's answer for each.
decodeRecords :: IO ()
decodeRecords = do
  root <- either error pure (resolveHuddle dijkstraCDDL)
  let spec = plainRoot root
      gen source k i = case source of
        "arb" -> arbBytes k i
        "cddl" -> cddlBytes HuddleEnv {heTwiddle = True, heRoot = root} k
        _ -> cddlBytes HuddleEnv {heTwiddle = False, heRoot = root} k
      generated =
        [ Out name (record name variant bs (Right d) ("rules" .= rulesOf spec k bs))
        | (name, k) <- decodeKinds
        , (source, n) <- sources
        , i <- [0 .. n - 1]
        , let bs = unGen (gen source k i) (mkQCGen (1 + i)) (1 + i `mod` 10)
              variant = T.pack (source <> " " <> show i)
        , Right d <- [reading k bs]
        ]
      outs = generated <> zapped root <> changed root unmarked
      tops = map (ruleOf . snd) changedKinds
      within = Mutation.reachable root (map Name tops)
      targets =
        [Aeson.object ["rule" .= r, "kinds" .= map fst wholeForms] | r <- tops]
          <> [ Aeson.object ["rule" .= unName n, "kinds" .= Mutation.kinds s]
             | (n, s) <- snd (Mutation.marked Set.empty root)
             , Set.member n within
             , not (null (Mutation.kinds s))
             ]
      extra = "targets" .= targets
  header "decode" (map fst decodeKinds <> sortOn T.toLower (nubOrd (drop (length generated) (map outClass outs)))) extra
  mapM_ (line . outRecord) outs

-- Networking records -------------------------------------------------------------

-- | The eras whose local state query UTxO replies the networking records hold.
data ReplyEra = ConwayReply | DijkstraReply

-- | The name of an era in a networking class.
replyEraName :: ReplyEra -> T.Text
replyEraName = \case
  ConwayReply -> "conway"
  DijkstraReply -> "dijkstra"

-- | The ledger's reading of a UTxO reply of an era, or its reason to refuse it.
replyReading :: ReplyEra -> BS.ByteString -> Either T.Text D
replyReading = \case
  ConwayReply -> utxoReading @ConwayEra timelockD
  DijkstraReply -> utxoReading @DijkstraEra nativeD

-- | The ledger's reading of a UTxO reply with the UTxO decoder of an era at its highest version.
utxoReading ::
  forall era.
  (BabbageEraTxOut era, Value era ~ MaryValue, Script era ~ AlonzoScript era, AlonzoEraScript era, DecCBOR (UTxO era)) =>
  (NativeScript era -> D) ->
  BS.ByteString ->
  Either T.Text D
utxoReading native bs = either (Left . refusal) (Right . dump) (decodeFull' @(UTxO era) (eraProtVerHigh @era) bs)
  where
    dump (UTxO m) = DO [("utxos", DL [DO [("input", c i), ("output", outD native o)] | (i, o) <- Map.toList m])]

-- | The outputs drawn of an era from seed 1 at sizes 1 to 30, each as the ledger writes it.
drawnOutputs :: ReplyEra -> Int -> [CBOR.Term]
drawnOutputs e n = [t | i <- [0 .. n - 1], Just t <- [termOf (unGen gen (mkQCGen (1 + i)) (1 + i `mod` 30))]]
  where
    gen = case e of
      ConwayReply -> serialize' (eraProtVerHigh @ConwayEra) <$> arbitrary @(TxOut ConwayEra)
      DijkstraReply -> serialize' ver <$> arbitrary @(TxOut DijkstraEra)

-- | The term of bytes that hold one data item and nothing after it.
termOf :: BS.ByteString -> Maybe CBOR.Term
termOf bs = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict bs) of
  Right (rest, t) | BSL.null rest -> Just t
  _ -> Nothing

-- | The value of an integer key of a map term.
entryOf :: Int -> CBOR.Term -> Maybe CBOR.Term
entryOf k = \case
  CBOR.TMap kvs -> lookup (CBOR.TInt k) kvs
  CBOR.TMapI kvs -> lookup (CBOR.TInt k) kvs
  _ -> Nothing

-- | The address and the value of an output term of either form.
addressValue :: CBOR.Term -> Maybe (CBOR.Term, CBOR.Term)
addressValue = \case
  CBOR.TList (a : v : _) -> Just (a, v)
  CBOR.TListI (a : v : _) -> Just (a, v)
  t -> (,) <$> entryOf 0 t <*> entryOf 1 t

-- | The script of a script reference term, its arm and its body.
scriptOf :: CBOR.Term -> Maybe CBOR.Term
scriptOf = \case
  CBOR.TTagged 24 (CBOR.TBytes bs) -> termOf bs
  _ -> Nothing

-- | The forms of output a UTxO reply's outputs take in the networking classes,
-- each making a map output of that form from the address and value entries,
-- given the scripts and datums the drawn outputs hold and the index of the
-- variant.
outputForms :: ([CBOR.Term] -> Int -> Maybe CBOR.Term) -> [CBOR.Term] -> [CBOR.Term] -> [CBOR.Term] -> [(T.Text, Int -> Maybe ([(CBOR.Term, CBOR.Term)] -> CBOR.Term))]
outputForms pick scripts hashes inlines =
  [(name, fmap adding . make) | (name, make) <- extraForms pick scripts hashes inlines]
    <> [ ("map-indefinite", \_ -> Just CBOR.TMapI)
       , ("no-address", \_ -> Just (CBOR.TMap . filter ((/= CBOR.TInt 0) . fst)))
       , ("no-value", \_ -> Just (CBOR.TMap . filter ((/= CBOR.TInt 1) . fst)))
       ]
  where
    adding extra entries = CBOR.TMap (entries <> extra)

-- | The forms of output that add entries to a map output, each with the
-- entries it adds, given the scripts and datums the drawn outputs hold and
-- the index of the variant.
extraForms :: ([CBOR.Term] -> Int -> Maybe CBOR.Term) -> [CBOR.Term] -> [CBOR.Term] -> [CBOR.Term] -> [(T.Text, Int -> Maybe [(CBOR.Term, CBOR.Term)])]
extraForms pick scripts hashes inlines =
  [ ("map-output", \_ -> Just [])
  , ("script-native", withScript . pick (armed 0))
  , ("script-plutus-v1", withScript . pick (armed 1))
  , ("script-plutus-v2", withScript . pick (armed 2))
  , ("script-plutus-v3", withScript . pick (armed 3))
  , ("script-plutus-v4", withScript . pick (armed 4))
  , ("script-unknown-arm", \i -> withScript (Just (CBOR.TList [CBOR.TInt 5, CBOR.TBytes (bytesOf 28 i)])))
  , ("datum-hash", withDatum . pick hashes)
  , ("datum-inline", withDatum . pick inlines)
  , ("datum-unknown-arm", \i -> withDatum (Just (CBOR.TList [CBOR.TInt 2, CBOR.TBytes (bytesOf 32 i)])))
  , ("unknown-key", \i -> Just [(CBOR.TInt 4, CBOR.TInt i)])
  ]
    <> [ (name, \i -> withScript (Just (CBOR.TList [CBOR.TInt 0, arm i])))
       | (name, arm) <- nativeForms
       ]
  where
    armed n = [s | s@(CBOR.TList (CBOR.TInt a : _)) <- scripts, a == n]
    withScript = fmap (\s -> [(CBOR.TInt 3, CBOR.TTagged 24 (CBOR.TBytes (CBOR.toStrictByteString (CBOR.encodeTerm s))))])
    withDatum = fmap (\d -> [(CBOR.TInt 2, d)])

-- | The arms of a native script, from the spec's `native_script` choice, and one arm it does not define, each made at the index of a variant.
nativeForms :: [(T.Text, Int -> CBOR.Term)]
nativeForms =
  [ ("native-pubkey", sig)
  , ("native-all", \i -> CBOR.TList [CBOR.TInt 1, CBOR.TList [sig i]])
  , ("native-any", \i -> CBOR.TList [CBOR.TInt 2, CBOR.TList [sig i]])
  , ("native-n-of-k", \i -> CBOR.TList [CBOR.TInt 3, CBOR.TInt 1, CBOR.TList [sig i]])
  , ("native-invalid-before", \i -> CBOR.TList [CBOR.TInt 4, CBOR.TInt (1000 * i)])
  , ("native-invalid-hereafter", \i -> CBOR.TList [CBOR.TInt 5, CBOR.TInt (1000 * i)])
  , ("native-require-guard", \i -> CBOR.TList [CBOR.TInt 6, CBOR.TList [CBOR.TInt 0, CBOR.TBytes (bytesOf 28 i)]])
  , ("native-unknown-arm", \i -> CBOR.TList [CBOR.TInt 7, CBOR.TBytes (bytesOf 28 i)])
  ]
  where
    sig i = CBOR.TList [CBOR.TInt 0, CBOR.TBytes (bytesOf 28 i)]

-- | A byte string of a length, every byte the low byte of an index.
bytesOf :: Int -> Int -> BS.ByteString
bytesOf n i = BS.replicate n (fromIntegral i)

-- | The number of outputs drawn of each era, and the number of variants of each networking class.
drawnOutputCount, replyVariants :: Int
drawnOutputCount = 400
replyVariants = 10

-- | Prints the networking records: for each era, UTxO replies of one output
-- of each form, the address and value of each from an output drawn of that
-- era, with the ledger's answer for each.
networkingRecords :: IO ()
networkingRecords = do
  let eras = [ConwayReply, DijkstraReply]
      drawnAll = concatMap (`drawnOutputs` drawnOutputCount) eras
      scripts = nubOrd [s | o <- drawnAll, Just r <- [entryOf 3 o], Just s <- [scriptOf r]]
      hashes = nubOrd [d | o <- drawnAll, Just d@(CBOR.TList (CBOR.TInt 0 : _)) <- [entryOf 2 o]]
      inlines = nubOrd [d | o <- drawnAll, Just d@(CBOR.TList (CBOR.TInt 1 : _)) <- [entryOf 2 o]]
      pick pool i = case drop (i `mod` max 1 (length pool)) pool of
        x : _ -> Just x
        [] -> Nothing
      input i = CBOR.TList [CBOR.TBytes (bytesOf 32 i), CBOR.TInt i]
      reply i o = CBOR.toStrictByteString (CBOR.encodeTerm (CBOR.TMap [(input i, o)]))
      rows e =
        let drawn' = drawnOutputs e drawnOutputCount
            legacy = [o | o <- drawn', isList o]
            pairs = [p | o <- drawn', Just p <- [addressValue o]]
            plainMap (a, v) = [(CBOR.TInt 0, a), (CBOR.TInt 1, v)]
            klass form = replyEraName e <> "-utxo " <> form
         in [ (klass "legacy-output", T.pack (show i), reply i o, Nothing)
            | (i, o) <- zip [0 ..] (take replyVariants legacy)
            ]
              <> [ (klass form, T.pack (show i), reply i (output (plainMap p)), Just (reply i (CBOR.TMap (plainMap p))))
                 | (form, make) <- outputForms pick scripts hashes inlines
                 , (i, p) <- zip [0 .. replyVariants - 1] (cycle pairs)
                 , Just output <- [make i]
                 ]
      isList = \case
        CBOR.TList _ -> True
        CBOR.TListI _ -> True
        _ -> False
      formNames = "legacy-output" : map fst (outputForms pick scripts hashes inlines)
      classes = [replyEraName e <> "-utxo " <> f | e <- eras, f <- formNames]
  header "networking" classes ("inputs" .= ("UTxO replies of one output, " <> T.pack (show replyVariants) <> " of each era and form" :: T.Text))
  mapM_
    ( \(e, (class_, variant, bs, base)) ->
        line (record class_ variant bs (replyReading e bs) (maybe mempty (("base" .=) . hexT) base))
    )
    [(e, r) | e <- eras, r <- rows e]

-- Mapping records ----------------------------------------------------------------

-- | Where the datum of an output of a sub transaction is.
data DatumSource = SubWitness | TopWitness | BothWitness | NoWitness | Inline | NoDatumSource
  deriving (Eq, Enum, Bounded)

-- | The name of a datum source in a mapping class.
sourceName :: DatumSource -> T.Text
sourceName = \case
  SubWitness -> "sub-witness"
  TopWitness -> "top-witness"
  BothWitness -> "both-witness"
  NoWitness -> "no-witness"
  Inline -> "inline"
  NoDatumSource -> "none"

-- | A transaction of two sub transactions whose sub at a position has as its
-- first output a drawn output with a datum of a source.
mappingTx :: DatumSource -> Int -> Gen (Tx TopTx DijkstraEra)
mappingTx source at = do
  top <- arbitrary @(Tx TopTx DijkstraEra)
  subs <- vectorOf 2 (arbitrary @(Tx SubTx DijkstraEra))
  out <- arbitrary @(TxOut DijkstraEra)
  d <- arbitrary @(Data DijkstraEra)
  let h = hashData d
      datum = case source of
        Inline -> Datum (dataToBinaryData d)
        NoDatumSource -> NoDatum
        _ -> DatumHash h
      witnessing :: Bool -> Tx l DijkstraEra -> Tx l DijkstraEra
      witnessing False tx = tx
      witnessing True tx = tx & witsTxL . datsTxWitsL .~ TxDats (Map.insert h d (unTxDats (tx ^. witsTxL . datsTxWitsL)))
      held k = k == at && source `elem` [SubWitness, BothWitness]
      placed k tx
        | k == at = tx & bodyTxL . outputsTxBodyL .~ ((out & datumTxOutL .~ datum) StrictSeq.<| (tx ^. bodyTxL . outputsTxBodyL))
        | otherwise = tx
      subs' = [witnessing (held k) (placed k s) | (k, s) <- zip [0 ..] subs]
  pure (witnessing (source `elem` [TopWitness, BothWitness]) top & bodyTxL . subTransactionsTxBodyL .~ OMap.fromFoldable subs')

-- | For each sub transaction of a transaction, each output with the source,
-- hash and bytes of the datum the ledger allows for it, from the witnesses
-- of that sub transaction or of the top transaction.
datumsD :: Tx TopTx DijkstraEra -> D
datumsD tx = DO [("subs", DL [DL [outputD s o | o <- toList (s ^. bodyTxL . outputsTxBodyL)] | s <- subs])]
  where
    subs = toList (tx ^. bodyTxL . subTransactionsTxBodyL)
    dats :: Tx l DijkstraEra -> Map.Map DataHash (Data DijkstraEra)
    dats t = unTxDats (t ^. witsTxL . datsTxWitsL)
    raw = DS . hexT . hashToBytes . extractHash
    found name h bytes = DO [("source", DS name), ("hash", raw h), ("datum", maybe DNull (DS . hexT) bytes)]
    outputD s o = case o ^. datumTxOutL of
      NoDatum -> DO [("source", DS (sourceName NoDatumSource)), ("hash", DNull), ("datum", DNull)]
      Datum bd -> found (sourceName Inline) (hashBinaryData bd) (Just (originalBytes bd))
      DatumHash h -> found (sourceName (witnessed s h)) h (originalBytes <$> Map.lookup h (dats s <> dats tx))
    witnessed s h = case (Map.member h (dats s), Map.member h (dats tx)) of
      (True, True) -> BothWitness
      (True, False) -> SubWitness
      (False, True) -> TopWitness
      (False, False) -> NoWitness

-- | The number of variants of each mapping class.
mappingVariants :: Int
mappingVariants = 10

-- | Prints the mapping records: for each datum source and sub position,
-- transactions whose sub at that position holds an output with a datum of
-- that source, with the ledger's datums for each.
mappingRecords :: IO ()
mappingRecords = do
  let positions = [(0, "first"), (1, "later")] :: [(Int, T.Text)]
      datumSources = [minBound .. maxBound]
      classes = [sourceName s <> " " <> p | s <- datumSources, (_, p) <- positions]
  header "mapping" classes ("inputs" .= ("transactions of two sub transactions, " <> T.pack (show mappingVariants) <> " of each source and position" :: T.Text))
  mapM_
    (\(class_, i, bs) -> line (record class_ (T.pack (show i)) bs (annotated @(Tx TopTx DijkstraEra) datumsD bs) mempty))
    [ (sourceName s <> " " <> p, i, serialize' ver (unGen (mappingTx s at) (mkQCGen (1 + i)) (1 + i `mod` 5)))
    | s <- datumSources
    , (at, p) <- positions
    , i <- [0 .. mappingVariants - 1]
    ]

-- | One transaction input of the validation records, with its class and variant.
data Case = Case {caseClass :: T.Text, caseVariant :: T.Text, caseInput :: BS.ByteString}

-- | Reads the inputs of a record file, each with its class and variant.
readCases :: FilePath -> IO [Case]
readCases path = concatMap parse . filter (not . BS8.null) . BS8.lines <$> BS.readFile path
  where
    parse l = either error id $ do
      v <- Aeson.eitherDecodeStrict l :: Either String Aeson.Object
      let field name = maybe (Left ("no " <> name)) Right (Aeson.parseMaybe (Aeson..: Key.fromString name) v)
      case field "input" of
        Left _ -> pure []
        Right hex -> (\k n i -> [Case k n i]) <$> field "class" <*> field "variant" <*> B16.decode (T.encodeUtf8 hex)

-- | The number of changes of the base transaction the validation records hold.
baseChangeDraws :: Int
baseChangeDraws = 1000

-- | The changes of the base transaction, each signed again, from seed 1.
baseChanges :: BS.ByteString -> [Case]
baseChanges base = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict base) of
  Right (_, t) ->
    [ Case ("generated " <> SignedChange.kindName k) (T.pack (show i)) (SignedChange.resign base bs)
    | i <- [0 .. baseChangeDraws - 1]
    , let (k, bs) = unGen (SignedChange.change t) (mkQCGen (1 + i)) 30
    ]
  Left e -> error ("the base transaction is not CBOR: " <> show e)

-- | Prints the validation records of the base and planted inputs of a record
-- file and of the changes of its base, the input classes in the order of
-- their names.
validationRecords :: FilePath -> IO ()
validationRecords path = do
  planted <- filter (not . T.isPrefixOf "generated " . caseClass) <$> readCases path
  base <- case [caseInput x | x <- planted, caseClass x == "base"] of
    [b] -> pure b
    _ -> error (path <> " holds no single base transaction")
  unless (SignedChange.signs base) $ error "the signer of the changes does not sign the base transaction"
  let cases = planted <> baseChanges base
      inputs = "the base and planted inputs of the committed validation records, and " <> T.pack (show baseChangeDraws) <> " changes of the base"
  header "validation" (sortOn T.toLower (nubOrd (map caseClass cases))) ("inputs" .= (inputs :: T.Text))
  mapM_ (\x -> line (record (caseClass x) (caseVariant x) (caseInput x) (reading KTx (caseInput x)) mempty)) cases

main :: IO ()
main =
  getArgs >>= \case
    ["decode"] -> decodeRecords
    ["validation", records] -> validationRecords records
    ["networking"] -> networkingRecords
    ["mapping"] -> mappingRecords
    _ -> error "usage: reference-writer decode | validation RECORDS | networking | mapping"
