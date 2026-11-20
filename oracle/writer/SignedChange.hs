{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}

-- | Changes to the structure of a signed transaction, each signed again by the key that signs the transaction.
module SignedChange (Kind, kindName, change, resign, signs) where

import Cardano.Binary.FixedSizeCodec (rawEncodeFixedSized)
import Cardano.Crypto.DSIGN (Ed25519DSIGN, SignKeyDSIGN, genKeyDSIGN, signDSIGN)
import Cardano.Crypto.Hash (Blake2b_256, hashToBytes, hashWith)
import Cardano.Crypto.Seed (mkSeedFromBytes)
import qualified Codec.CBOR.Decoding as D
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Term as CBOR
import qualified Codec.CBOR.Write as CBOR
import Data.Bits (shiftR)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BSL
import qualified Data.Text as T
import Test.QuickCheck (Gen, choose, elements, shuffle)

-- | A change to the structure of a term.
data Kind = Drop | Extra | DupKey | UnknownKey | SwapType | Negate | Oversize | Trailing
  deriving (Eq, Show, Enum, Bounded)

-- | The name of a kind, which its class carries.
kindName :: Kind -> T.Text
kindName = \case
  Drop -> "drop"
  Extra -> "extra"
  DupKey -> "dupkey"
  UnknownKey -> "unknownkey"
  SwapType -> "swaptype"
  Negate -> "negate"
  Oversize -> "oversize"
  Trailing -> "trailing"

-- | The terms a term holds, the keys and values of a map in turn.
children :: CBOR.Term -> [CBOR.Term]
children = \case
  CBOR.TList ts -> ts
  CBOR.TListI ts -> ts
  CBOR.TMap kvs -> concatMap (\(k, v) -> [k, v]) kvs
  CBOR.TMapI kvs -> concatMap (\(k, v) -> [k, v]) kvs
  CBOR.TTagged _ t -> [t]
  _ -> []

-- | Every term in a term with its path, parents before children.
nodes :: CBOR.Term -> [([Int], CBOR.Term)]
nodes t = ([], t) : [(i : p, n) | (i, c) <- zip [0 ..] (children t), (p, n) <- nodes c]

-- | The term with the term at a path replaced.
replaceAt :: [Int] -> CBOR.Term -> CBOR.Term -> CBOR.Term
replaceAt [] new _ = new
replaceAt (i : p) new t = case t of
  CBOR.TList ts -> CBOR.TList (items ts)
  CBOR.TListI ts -> CBOR.TListI (items ts)
  CBOR.TMap kvs -> CBOR.TMap (pairs kvs)
  CBOR.TMapI kvs -> CBOR.TMapI (pairs kvs)
  CBOR.TTagged n x -> CBOR.TTagged n (replaceAt p new x)
  _ -> t
  where
    one j x = if j == i then replaceAt p new x else x
    items ts = zipWith one [0 ..] ts
    pairs kvs = [(one (2 * j) a, one (2 * j + 1) b) | (j, (a, b)) <- zip [0 ..] kvs]

-- | The elements of a list and the pairs of a map, each changed by its function.
overItems :: ([CBOR.Term] -> [CBOR.Term]) -> ([(CBOR.Term, CBOR.Term)] -> [(CBOR.Term, CBOR.Term)]) -> CBOR.Term -> CBOR.Term
overItems f g = \case
  CBOR.TList ts -> CBOR.TList (f ts)
  CBOR.TListI ts -> CBOR.TListI (f ts)
  CBOR.TMap kvs -> CBOR.TMap (g kvs)
  CBOR.TMapI kvs -> CBOR.TMapI (g kvs)
  t -> t

-- | The number of elements of a list or pairs of a map.
size :: CBOR.Term -> Int
size = \case
  CBOR.TList ts -> length ts
  CBOR.TListI ts -> length ts
  CBOR.TMap kvs -> length kvs
  CBOR.TMapI kvs -> length kvs
  _ -> 0

-- | Whether a term is a list, and whether it is a map.
isList, isMap :: CBOR.Term -> Bool
isList = \case
  CBOR.TList _ -> True
  CBOR.TListI _ -> True
  _ -> False
isMap = \case
  CBOR.TMap _ -> True
  CBOR.TMapI _ -> True
  _ -> False

-- | The value of an integer term.
integer :: CBOR.Term -> Maybe Integer
integer = \case
  CBOR.TInt n -> Just (toInteger n)
  CBOR.TInteger n -> Just n
  _ -> Nothing

-- | The value of a byte string term.
bytes :: CBOR.Term -> Maybe BS.ByteString
bytes = \case
  CBOR.TBytes b -> Just b
  CBOR.TBytesI b -> Just (BSL.toStrict b)
  _ -> Nothing

-- | Whether a kind can change a term.
admits :: Kind -> CBOR.Term -> Bool
admits k t = case k of
  Drop -> (isList t || isMap t) && size t > 0
  Extra -> isList t || isMap t
  DupKey -> isMap t && size t > 0
  UnknownKey -> isMap t
  SwapType -> integer t /= Nothing || bytes t /= Nothing
  Negate -> integer t /= Nothing
  Oversize -> integer t /= Nothing
  Trailing -> False

-- | A term a kind makes of a term it admits.
changeTerm :: Kind -> CBOR.Term -> Gen CBOR.Term
changeTerm k t = case k of
  Drop -> do
    j <- choose (0, size t - 1)
    pure (overItems (without j) (without j) t)
  Extra -> pure (overItems (<> [CBOR.TInt 0]) (<> [unknown]) t)
  DupKey -> do
    j <- choose (0, size t - 1)
    pure (overItems id (\kvs -> kvs <> take 1 (drop j kvs)) t)
  UnknownKey -> pure (overItems id (<> [unknown]) t)
  SwapType -> pure $ case (integer t, bytes t) of
    (Just n, _) -> CBOR.TBytes (BS.pack [fromInteger (abs n `shiftR` (8 * s)) | s <- [7, 6 .. 0]])
    (_, b) -> CBOR.TInteger (BS.foldl' (\a w -> a * 256 + toInteger w) 0 (BS.take 8 (maybe BS.empty id b)))
  Negate -> pure (maybe t (\n -> CBOR.TInteger (-1 - n)) (integer t))
  Oversize -> pure (maybe t (\n -> CBOR.TInteger (2 ^ (70 :: Int) + abs n)) (integer t))
  Trailing -> pure t
  where
    without j xs = take j xs <> drop (j + 1) xs
    unknown = (CBOR.TInt 1000, CBOR.TInt 0)

-- | One change of a transaction's term, its kind drawn among those some term in it admits, with the encoding it makes.
change :: CBOR.Term -> Gen (Kind, BS.ByteString)
change t = do
  ks <- shuffle [minBound .. maxBound]
  case [(k, ps) | k <- ks, let ps = [(p, n) | (p, n) <- nodes t, admits k n], k == Trailing || not (null ps)] of
    (Trailing, _) : _ -> pure (Trailing, encoded t <> BS.singleton 0)
    (k, ps) : _ -> do
      (p, n) <- elements ps
      n' <- changeTerm k n
      pure (k, encoded (replaceAt p n' t))
    [] -> pure (Trailing, encoded t <> BS.singleton 0)
  where
    encoded = CBOR.toStrictByteString . CBOR.encodeTerm

-- | The key that signs the planted transactions, from the seed of 32 sevens.
signer :: SignKeyDSIGN Ed25519DSIGN
signer = genKeyDSIGN (mkSeedFromBytes (BS.replicate 32 7))

-- | The encoding of the first item of an array.
firstItem :: BS.ByteString -> Maybe BS.ByteString
firstItem bs = case CBOR.deserialiseFromBytes span_ (BSL.fromStrict bs) of
  Right (_, (s, e)) -> Just (BS.take (fromIntegral (e - s)) (BS.drop (fromIntegral s) bs))
  Left _ -> Nothing
  where
    span_ = D.decodeListLenOrIndef *> ((,) <$> D.peekByteOffset <* CBOR.decodeTerm <*> D.peekByteOffset)

-- | The signature of the first key witness of a transaction.
signatureOf :: BS.ByteString -> Maybe BS.ByteString
signatureOf tx = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict tx) of
  Right (_, CBOR.TList (_ : CBOR.TMap kvs : _))
    | Just ws <- lookup (CBOR.TInt 0) kvs
    , CBOR.TList (_ : CBOR.TBytes sig : _) : _ <- elementsOf ws ->
        Just sig
  _ -> Nothing
  where
    elementsOf = \case
      CBOR.TTagged _ x -> elementsOf x
      CBOR.TList xs -> xs
      CBOR.TListI xs -> xs
      _ -> []

-- | The offsets at which a string occurs in another.
occurrences :: BS.ByteString -> BS.ByteString -> [Int]
occurrences needle = go 0
  where
    go off hay = case BS.breakSubstring needle hay of
      (pre, rest)
        | BS.null rest -> []
        | otherwise -> let i = off + BS.length pre in i : go (i + 1) (BS.drop (BS.length pre + 1) hay)

-- | The signer's signature over the hash of a transaction body.
signature :: BS.ByteString -> BS.ByteString
signature body = rawEncodeFixedSized (signDSIGN () (hashToBytes (hashWith @Blake2b_256 id body)) signer)

-- | Whether the first key witness of a transaction is the signer's signature over its first item.
signs :: BS.ByteString -> Bool
signs tx = case (firstItem tx, signatureOf tx) of
  (Just body, Just sig) -> signature body == sig
  _ -> False

-- | A changed transaction with the signature of a base transaction, when it occurs once in it, replaced by the signer's signature over the changed transaction's first item.
resign :: BS.ByteString -> BS.ByteString -> BS.ByteString
resign base tx = case (firstItem tx, signatureOf base) of
  (Just body, Just old) | [i] <- occurrences old tx -> BS.take i tx <> signature body <> BS.drop (i + BS.length old) tx
  _ -> tx
