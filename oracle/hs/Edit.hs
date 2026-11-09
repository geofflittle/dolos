{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Transactions whose bytes are edited after the fixups, with their hashes recomputed and their key witnesses signed again.
module Edit (
  Edit (..),
  noEdit,
  firstSub,
  submitEdited,
  editTx,
  editBytes,
  encoded,
  emptySet,
  entries,
  lookupKey,
  setKey,
  deleteKey,
  dupKey,
  addEntry,
  overKey,
  dupFirst,
  overFirst,
  overFirstValue,
  overIndex,
  everywhere,
) where

import Cardano.Crypto.Hash (castHash, hashToBytes, hashWith)
import Cardano.Ledger.Alonzo.PParams (encodeLangViews, getLanguageView)
import Cardano.Ledger.BaseTypes (pvMajor)
import Cardano.Ledger.Binary (EncCBOR, serialize')
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Hashes (HASH, unsafeMakeSafeHash)
import Cardano.Ledger.Keys (KeyHash, KeyRole (Witness), witVKeyHash)
import Cardano.Ledger.Plutus.Language (Language)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term (..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (void, zipWithM)
import Control.Monad.IO.Class (liftIO)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Foldable (toList)
import Data.IORef (writeIORef)
import Data.List (find, subsequences)
import qualified Data.Map.Strict as Map
import qualified Data.OMap.Strict as OMap
import qualified Data.Set as Set
import Dump (E, decodeTx, editedBytes)
import Lens.Micro ((^.))
import Lens.Micro.Mtl (use)
import Test.Cardano.Ledger.Core.KeyPair (KeyPair, mkWitnessesVKey)
import Test.Cardano.Ledger.Dijkstra.ImpTest

-- | Changes to the terms of a top level transaction, each of its sub transactions by index, and bytes after it.
data Edit = Edit
  { editBody :: Term -> Term
  , editWits :: Term -> Term
  , editAux :: Term -> Term
  , editSubBody :: Int -> Term -> Term
  , editSubWits :: Int -> Term -> Term
  , trailing :: BS.ByteString
  }

-- | The edit that changes nothing.
noEdit :: Edit
noEdit = Edit id id id (const id) (const id) ""

-- | A change to the sub transaction at index zero only.
firstSub :: (Term -> Term) -> Int -> Term -> Term
firstSub f 0 = f
firstSub _ _ = id

-- | Submits the transaction after the fixups and the edit, so the node's decoder judges the edited bytes.
submitEdited :: Edit -> Tx TopTx E -> ImpTestM E ()
submitEdited e tx = void $ withPostFixup (editTx e) (trySubmitTx tx)

-- | The fixed up transaction with the edit applied, decoded again when the node's decoder accepts the bytes.
editTx :: Edit -> Tx TopTx E -> ImpTestM E (Tx TopTx E)
editTx e tx = do
  pp <- getsPParams id
  pairs <- use impKeyPairsG
  let bytes = either error id (editBytes pp (`Map.lookup` pairs) e tx (termOf tx))
  case decodeTx (pvMajor (pp ^. ppProtocolVersionL)) bytes of
    Right edited -> pure edited
    Left _ -> do
      liftIO (writeIORef editedBytes (Just bytes))
      pure tx

-- | The bytes of a transaction's term with the edit applied, its hashes recomputed and the key witnesses of each changed body signed again, or why one cannot be.
editBytes :: PParams E -> (KeyHash Witness -> Maybe (KeyPair Witness)) -> Edit -> Tx TopTx E -> Term -> Either String BS.ByteString
editBytes pp keyPair e tx term = do
  (b0, w0, middle, a0) <- case term of
    TList [b, w, v, a] -> Right (b, w, [v], a)
    TList [b, w, a] -> Right (b, w, [], a)
    other -> Left ("a transaction of an unknown shape " <> show other)
  let a1 = editAux e a0
      b1 = if a1 == a0 then b0 else setKey 7 (TBytes (blake (encoded a1))) b0
      subKeys = map keyHashes (OMap.elems (tx ^. bodyTxL . subTransactionsTxBodyL))
  b2 <- case lookupKey 23 b1 of
    Just subs | not (null subKeys) -> (\s -> setKey 23 s b1) <$> overElems (\j -> editSub j (drop j subKeys)) subs
    _ -> Right b1
  let b3 = editBody e b2
  b4 <- integrity pp b3 w0 (editWits e w0)
  w1 <- editWits e <$> (if b4 == b0 then Right w0 else resign (keyHashes tx) b4 w0)
  pure (encoded (TList ([b4, w1] <> middle <> [a1])) <> trailing e)
  where
    resign hashes body w
      | null hashes = Right w
      | otherwise = do
          keys <- mapM (\h -> maybe (Left ("no key pair for " <> show h)) Right (keyPair h)) hashes
          Right (setKey 0 (termOf (mkWitnessesVKey (unsafeMakeSafeHash (castHash (hashWith @HASH id (encoded body)))) keys)) w)
    editSub j hashes sub = case sub of
      TList (sb : sw : rest) -> do
        let sb1 = editSubBody e j sb
        sb2 <- integrity pp sb1 sw (editSubWits e j sw)
        sw1 <- if sb2 == sb then Right sw else resign (concat (take 1 hashes)) sb2 sw
        Right (TList (sb2 : editSubWits e j sw1 : rest))
      other -> Left ("a sub transaction of an unknown shape " <> show other)

-- | The hashes of the keys that sign a transaction's key witnesses.
keyHashes :: Tx l E -> [KeyHash Witness]
keyHashes tx = map witVKeyHash (toList (tx ^. witsTxL . addrTxWitsL))

-- | The body with its script integrity hash recomputed over the edited witnesses, keeping the language views the old hash covers.
integrity :: PParams E -> Term -> Term -> Term -> Either String Term
integrity pp body old new = case lookupKey 11 body of
  Just (TBytes hash) | old /= new -> case find (\views -> blake (preimage old views) == hash) candidates of
    Just views -> Right (setKey 11 (TBytes (blake (preimage new views))) body)
    Nothing -> Left "no set of language views gives the script integrity hash"
  _ -> Right body
  where
    candidates = [serialize' (pvMajor (pp ^. ppProtocolVersionL)) (encodeLangViews (Set.fromList (map (getLanguageView pp) langs))) | langs <- subsequences [minBound .. maxBound :: Language]]
    preimage w views = maybe (encoded (TMap [])) encoded (lookupKey 5 w) <> maybe "" encoded (lookupKey 4 w) <> views

-- | The Blake2b 256 digest of bytes.
blake :: BS.ByteString -> BS.ByteString
blake = hashToBytes . hashWith @HASH id

-- | The bytes of a term.
encoded :: Term -> BS.ByteString
encoded = toStrictByteString . encodeTerm

-- | The term of a value's encoding at the highest protocol version of the era.
termOf :: EncCBOR a => a -> Term
termOf x = case deserialiseFromBytes decodeTerm (BL.fromStrict (serialize' (eraProtVerHigh @E) x)) of
  Right (rest, t) | BL.null rest -> t
  _ -> error "an encoding that is not one CBOR term"

-- | A tagged empty set.
emptySet :: Term
emptySet = TTagged 258 (TList [])

-- | The entries of a map term.
entries :: Term -> [(Term, Term)]
entries (TMap kvs) = kvs
entries (TMapI kvs) = kvs
entries _ = []

-- | The value at an integer key of a map term.
lookupKey :: Int -> Term -> Maybe Term
lookupKey k t = lookup (TInt k) (entries t)

-- | The map term with the value at the key replaced, or added at the end when the key is absent.
setKey :: Int -> Term -> Term -> Term
setKey k v t
  | any ((== TInt k) . fst) (entries t) = TMap [(key, if key == TInt k then v else x) | (key, x) <- entries t]
  | otherwise = addEntry (TInt k) v t

-- | The map term without the entry at the key.
deleteKey :: Int -> Term -> Term
deleteKey k t = TMap (filter ((/= TInt k) . fst) (entries t))

-- | The map term with the entry at the key given again at the end.
dupKey :: Int -> Term -> Term
dupKey k t = maybe t (\v -> addEntry (TInt k) v t) (lookupKey k t)

-- | The map term with an entry added at the end.
addEntry :: Term -> Term -> Term -> Term
addEntry k v t = TMap (entries t <> [(k, v)])

-- | The map term with the value at the key changed.
overKey :: Int -> (Term -> Term) -> Term -> Term
overKey k f t = maybe t (\v -> setKey k (f v) t) (lookupKey k t)

-- | A list or set term with each element changed by its index, or the first failure.
overElems :: (Int -> Term -> Either String Term) -> Term -> Either String Term
overElems f (TTagged n t) = TTagged n <$> overElems f t
overElems f (TList xs) = TList <$> zipWithM f [0 ..] xs
overElems f (TListI xs) = TListI <$> zipWithM f [0 ..] xs
overElems _ t = Right t

-- | A list, set or map term with its first element given again after it.
dupFirst :: Term -> Term
dupFirst (TTagged n t) = TTagged n (dupFirst t)
dupFirst (TList (x : xs)) = TList (x : x : xs)
dupFirst (TListI (x : xs)) = TListI (x : x : xs)
dupFirst (TMap (x : xs)) = TMap (x : x : xs)
dupFirst t = t

-- | A list or set term with its first element changed.
overFirst :: (Term -> Term) -> Term -> Term
overFirst f (TTagged n t) = TTagged n (overFirst f t)
overFirst f (TList (x : xs)) = TList (f x : xs)
overFirst f (TListI (x : xs)) = TListI (f x : xs)
overFirst _ t = t

-- | A map term with the value of its first entry changed.
overFirstValue :: (Term -> Term) -> Term -> Term
overFirstValue f (TMap ((k, v) : kvs)) = TMap ((k, f v) : kvs)
overFirstValue _ t = t

-- | A list term with the element at an index changed.
overIndex :: Int -> (Term -> Term) -> Term -> Term
overIndex i f (TList xs) = TList [if j == i then f x else x | (j, x) <- zip [0 ..] xs]
overIndex _ _ t = t

-- | A term with a change applied to every term inside it, innermost first.
everywhere :: (Term -> Term) -> Term -> Term
everywhere f t = f $ case t of
  TList xs -> TList (map (everywhere f) xs)
  TListI xs -> TListI (map (everywhere f) xs)
  TMap kvs -> TMap [(everywhere f k, everywhere f v) | (k, v) <- kvs]
  TMapI kvs -> TMapI [(everywhere f k, everywhere f v) | (k, v) <- kvs]
  TTagged n x -> TTagged n (everywhere f x)
  other -> other
