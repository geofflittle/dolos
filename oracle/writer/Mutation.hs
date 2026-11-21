{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The shapes of the spec's trees that a change applies to, the changes of each, and the CBOR writer that makes one change at a position.
module Mutation (
  Shape (..),
  Fraction (..),
  Change (..),
  shapeOf,
  kinds,
  changes,
  write,
  reachable,
) where

import Codec.CBOR.Cuddle.CDDL (Name (..), RangeBound (..), Value (..), ValueVariant (..))
import Codec.CBOR.Cuddle.CDDL.CTree (CTree (..), CTreeRoot (..), PTerm (..))
import Codec.CBOR.Cuddle.CDDL.CtlOp (CtlOp (Size))
import Codec.CBOR.Cuddle.CDDL.Resolve (MonoSimplePhase, XXCTree (..))
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Term as CBOR
import qualified Codec.CBOR.Write as CBOR
import qualified Data.ByteString as BS
import qualified Data.ByteString.Builder as B
import qualified Data.ByteString.Lazy as BSL
import Data.List (isPrefixOf)
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as T
import Data.Word (Word64, Word8)
import Path (Tree)

-- | The shape of a tree whose values a change applies to.
data Shape
  = -- | A choice whose arms each start with a distinct unsigned literal, and whether every arm is an array.
    Sum [Word64] Bool
  | -- | A set or ordered set of elements.
    SetOf
  | -- | A rational number under tag 30.
    Rational Fraction
  | -- | An array of fields.
    ArrayRecord
  | -- | A map of fields under the unsigned keys it declares.
    MapRecord [Word64]
  | -- | An integer between two bounds, the upper one absent when unbounded.
    Between Integer (Maybe Integer)
  | -- | Bytes or text whose length is between two bounds, the upper one absent when unbounded.
    Length Integer (Maybe Integer)
  deriving (Eq, Show)

-- | Which rational numbers a fraction holds.
data Fraction
  = -- | Any numerator over any denominator its rule allows.
    Unbounded
  | -- | A number from zero to one, the bound `unit_interval` states only in a comment of the CDDL.
    Unit
  deriving (Eq, Show)

-- | The kinds of change every value of the shape receives.
kinds :: Shape -> [T.Text]
kinds = \case
  Sum _ arrays -> "unknown-arm" : ["array-extra" | arrays] <> ["array-short" | arrays]
  SetOf -> ["set-duplicate", "set-two-ways", "set-reorder"]
  Rational f ->
    ["rational-tag", "rational-third", "rational-unreduced", "rational-one", "rational-empty", "rational-indefinite"]
      <> ["unit-above" | f == Unit]
      <> ["unit-zero-denominator" | f == Unit]
  ArrayRecord -> ["array-extra", "array-short"]
  MapRecord _ -> ["map-unknown-key"]
  Between _ hi -> "range-below" : ["range-above" | Just _ <- [hi]]
  Length lo hi -> ["range-below" | lo > 0] <> ["range-above" | Just _ <- [hi]]

-- | A change to one value: its kind, which case of the kind it is, a detail for the record's variant, and the change itself.
data Change = Change
  { changeKind :: T.Text
  , changeCase :: T.Text
  , changeDetail :: T.Text
  , changeApply :: CBOR.Term -> CBOR.Term
  }

-- | The tag under which the writer writes every integer and length in eight bytes.
wideTag :: Word64
wideTag = 0xFFFFFFFEFFFFFFFF

-- | The shape of a tree, given the rule it defines if any, when a change applies to its values.
shapeOf :: CTreeRoot MonoSimplePhase -> Maybe Name -> Tree -> Maybe Shape
shapeOf (CTreeRoot m) name tree
  | Just n <- name, set n = Just SetOf
  | otherwise = case tree of
      Tag 30 _ -> Just (Rational (if fmap unName name == Just "unit_interval" then Unit else Unbounded))
      Choice arms -> (\ks -> Sum ks (all array (NE.toList arms))) <$> traverse (leading m) (NE.toList arms)
      Map xs@(_ : _) -> MapRecord <$> traverse literalKey xs
      Array xs | not (any repeats xs) -> Just ArrayRecord
      Range a b closing -> do
        lo <- bound m a
        hi <- bound m b
        Just (Between lo (Just (if closing == Closed then hi else hi - 1)))
      Control Size sized size -> do
        (lo, hi) <- case size of
          Range a b closing -> (,) <$> bound m a <*> ((\h -> if closing == Closed then h else h - 1) <$> bound m b)
          x -> (\n -> (n, n)) <$> bound m x
        case sized of
          Postlude PTBytes -> Just (Length lo (Just hi))
          Postlude PTText -> Just (Length lo (Just hi))
          Postlude PTUInt -> Just (Between 0 (Just (256 ^ hi - 1)))
          _ -> Nothing
      _ -> Nothing
  where
    set r = any (`T.isPrefixOf` unName r) ["%set<", "%nonempty_set<", "%oset<", "%nonempty_oset<"]
    repeats = \case
      Occur {} -> True
      _ -> False
    array = \case
      Array _ -> True
      _ -> False
    literalKey = \case
      Occur x _ -> literalKey x
      KV (Literal (Value (VUInt k) _)) _ _ -> Just k
      _ -> Nothing

-- | The integer a bound of a range or size names.
bound :: Map.Map Name Tree -> Tree -> Maybe Integer
bound m = go (8 :: Int)
  where
    go 0 = const Nothing
    go d = \case
      Literal (Value (VUInt k) _) -> Just (toInteger k)
      Literal (Value (VNInt k) _) -> Just (negate (toInteger k))
      Literal (Value (VBignum k) _) -> Just k
      CTreeE (MSimpleRef r) -> Map.lookup r m >>= go (d - 1)
      _ -> Nothing

-- | The unsigned literal an arm of a choice starts with, when the arm is one value.
leading :: Map.Map Name Tree -> Tree -> Maybe Word64
leading m = \case
  Literal (Value (VUInt k) _) -> Just k
  Array (x : _) -> first (8 :: Int) x
  _ -> Nothing
  where
    first 0 = const Nothing
    first d = \case
      Literal (Value (VUInt k) _) -> Just k
      Group (x : _) -> first (d - 1) x
      CTreeE (MSimpleRef r) -> Map.lookup r m >>= first (d - 1)
      _ -> Nothing

-- | Writes a term, applying the change to the value at the position given, every head in eight bytes when wide, and reading again the bytes at the embedded positions that hold the change or are written wide.
write :: Set.Set [Int] -> Maybe ([Int], CBOR.Term -> CBOR.Term) -> Bool -> CBOR.Term -> BS.ByteString
write embedded wanted wide0 t0 = BSL.toStrict (B.toLazyByteString (go [] wide0 t0))
  where
    go pos wide t = emit pos wide (case wanted of Just (p, f) | p == pos -> f t; _ -> t)
    emit pos wide = \case
      CBOR.TTagged tag x
        | tag == wideTag -> emit pos True x
        | otherwise -> header wide 6 tag <> go (pos <> [0]) wide x
      CBOR.TList xs -> header wide 4 (fromIntegral (length xs)) <> list pos wide xs
      CBOR.TListI xs -> B.word8 0x9f <> list pos wide xs <> B.word8 0xff
      CBOR.TMap kvs -> header wide 5 (fromIntegral (length kvs)) <> pairs pos wide kvs
      CBOR.TMapI kvs -> B.word8 0xbf <> pairs pos wide kvs <> B.word8 0xff
      CBOR.TBytes bs
        | Set.member pos embedded
        , wide || holds pos
        , Right (rest, nested) <- CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict bs)
        , BSL.null rest ->
            let b = BSL.toStrict (B.toLazyByteString (go (pos <> [0]) wide nested))
             in header wide 2 (fromIntegral (BS.length b)) <> B.byteString b
      CBOR.TBytes bs | wide -> header wide 2 (fromIntegral (BS.length bs)) <> B.byteString bs
      CBOR.TString s | wide -> let bs = T.encodeUtf8 s in header wide 3 (fromIntegral (BS.length bs)) <> B.byteString bs
      CBOR.TInt i | wide -> integer wide (toInteger i)
      CBOR.TInteger i | wide, i >= negate (2 ^ (64 :: Int)), i < 2 ^ (64 :: Int) -> integer wide i
      leaf -> CBOR.toBuilder (CBOR.encodeTerm leaf)
    list pos wide xs = mconcat [go (pos <> [k]) wide x | (k, x) <- zip [0 ..] xs]
    pairs pos wide kvs = mconcat [go (pos <> [2 * k]) wide kt <> go (pos <> [2 * k + 1]) wide v | (k, (kt, v)) <- zip [0 ..] kvs]
    holds pos = case wanted of
      Just (p, _) -> (pos <> [0]) `isPrefixOf` p
      Nothing -> False

-- | The head of a data item, in eight bytes when wide.
header :: Bool -> Word8 -> Word64 -> B.Builder
header wide major v
  | wide = B.word8 (major * 32 + 27) <> B.word64BE v
  | v < 24 = B.word8 (major * 32 + fromIntegral v)
  | v < 0x100 = B.word8 (major * 32 + 24) <> B.word8 (fromIntegral v)
  | v < 0x10000 = B.word8 (major * 32 + 25) <> B.word16BE (fromIntegral v)
  | v < 0x100000000 = B.word8 (major * 32 + 26) <> B.word32BE (fromIntegral v)
  | otherwise = B.word8 (major * 32 + 27) <> B.word64BE v

-- | An integer of the range of a head.
integer :: Bool -> Integer -> B.Builder
integer wide i
  | i >= 0 = header wide 0 (fromInteger i)
  | otherwise = header wide 1 (fromInteger (negate 1 - i))

-- | The rules a value of any of the named rules can contain, those included.
reachable :: CTreeRoot MonoSimplePhase -> [Name] -> Set.Set Name
reachable (CTreeRoot m) = go Set.empty
  where
    go seen [] = seen
    go seen (n : rest)
      | Set.member n seen = go seen rest
      | otherwise = go (Set.insert n seen) (maybe [] refs (Map.lookup n m) <> rest)
    refs = \case
      Literal _ -> []
      Postlude _ -> []
      Map xs -> concatMap refs xs
      Array xs -> concatMap refs xs
      Choice xs -> concatMap refs (NE.toList xs)
      Group xs -> concatMap refs xs
      KV k v _ -> refs k <> refs v
      Occur x _ -> refs x
      Range a b _ -> refs a <> refs b
      Control _ a b -> refs a <> refs b
      Enum x -> refs x
      Unwrap x -> refs x
      Tag _ x -> refs x
      CTreeE (MSimpleRef r) -> [r]

-- | The changes that apply to a value of a shape.
changes :: Shape -> CBOR.Term -> [Change]
changes shape t = case shape of
  Sum ks allArrays -> unknownArms ks <> (if allArrays then arrays else [])
  ArrayRecord -> arrays
  MapRecord ks -> case t of
    CBOR.TMap kvs ->
      [ Change "map-unknown-key" case_ (T.pack (show k)) (const (CBOR.TMap (kvs <> [(CBOR.TInteger (toInteger k), CBOR.TInt 0)])))
      | (case_, k) <- [("above", maximum ks + 1)] <> take 1 [("gap", k) | k <- [0 .. maximum ks], k `notElem` ks]
      ]
    _ -> []
  SetOf -> case elementsOf t of
    Just (wrap, xs@(x : _)) ->
      [Change "set-duplicate" "" "" (const (wrap (xs <> [x])))]
        <> [Change "set-two-ways" "" "" (const (wrap (xs <> [CBOR.TTagged wideTag x])))]
        <> [Change "set-reorder" "" "" (const (wrap (reverse xs))) | reverse xs /= xs]
    _ -> []
  Rational f -> case t of
    CBOR.TTagged 30 (CBOR.TList [n, d]) ->
      [ Change "rational-tag" "" "31" (const (CBOR.TTagged 31 (CBOR.TList [n, d])))
      , Change "rational-third" "" "" (const (CBOR.TTagged 30 (CBOR.TList [n, d, CBOR.TInt 1])))
      , Change "rational-one" "" "" (const (CBOR.TTagged 30 (CBOR.TList [n])))
      , Change "rational-empty" "" "" (const (CBOR.TTagged 30 (CBOR.TList [])))
      , Change "rational-indefinite" "" "" (const (CBOR.TTagged 30 (CBOR.TListI [n, d])))
      ]
        <> [ Change "rational-unreduced" "" "" (const (CBOR.TTagged 30 (CBOR.TList [CBOR.TInteger (2 * a), CBOR.TInteger (2 * b)])))
           | Just a <- [intOf n]
           , Just b <- [intOf d]
           , 2 * max a b < 2 ^ (64 :: Int)
           ]
        <> [ c
           | f == Unit
           , c <-
              [ Change "unit-above" "" "2/1" (const (CBOR.TTagged 30 (CBOR.TList [CBOR.TInt 2, CBOR.TInt 1])))
              , Change "unit-zero-denominator" "" "1/0" (const (CBOR.TTagged 30 (CBOR.TList [CBOR.TInt 1, CBOR.TInt 0])))
              ]
           ]
    _ -> []
  Between lo hi -> case intOf t of
    Just _ ->
      [Change "range-below" "" (T.pack (show (lo - 1))) (const (CBOR.TInteger (lo - 1)))]
        <> [Change "range-above" "" (T.pack (show (h + 1))) (const (CBOR.TInteger (h + 1))) | Just h <- [hi]]
    Nothing -> []
  Length lo hi -> case t of
    CBOR.TBytes bs ->
      [Change "range-below" "" (T.pack (show (lo - 1))) (const (CBOR.TBytes (sized (lo - 1) bs))) | lo > 0]
        <> [Change "range-above" "" (T.pack (show (h + 1))) (const (CBOR.TBytes (sized (h + 1) bs))) | Just h <- [hi]]
    CBOR.TString _ ->
      [Change "range-below" "" (T.pack (show (lo - 1))) (const (CBOR.TString (T.replicate (fromInteger (lo - 1)) "a"))) | lo > 0]
        <> [Change "range-above" "" (T.pack (show (h + 1))) (const (CBOR.TString (T.replicate (fromInteger (h + 1)) "a"))) | Just h <- [hi]]
    _ -> []
  where
    unknownArms ks =
      [ Change "unknown-arm" "" (T.pack (show u)) (setLeading u)
      | Just _ <- [leadingOf t]
      , u <- [k | k <- [0 .. maximum ks + 1], k `notElem` ks]
      ]
    arrays = case t of
      CBOR.TList xs@(_ : _) ->
        [Change "array-extra" "" "" (const (CBOR.TList (xs <> [CBOR.TInt 0]))), Change "array-short" "" "" (const (CBOR.TList (init xs)))]
      _ -> []
    setLeading u = \case
      CBOR.TList (_ : xs) -> CBOR.TList (CBOR.TInteger (toInteger u) : xs)
      CBOR.TListI (_ : xs) -> CBOR.TListI (CBOR.TInteger (toInteger u) : xs)
      _ -> CBOR.TInteger (toInteger u)
    leadingOf = \case
      CBOR.TList (x : _) -> intOf x
      CBOR.TListI (x : _) -> intOf x
      x -> intOf x
    intOf = \case
      CBOR.TInt i -> Just (toInteger i)
      CBOR.TInteger i -> Just i
      _ -> Nothing
    elementsOf = \case
      CBOR.TTagged 258 (CBOR.TList xs) -> Just (CBOR.TTagged 258 . CBOR.TList, xs)
      CBOR.TTagged 258 (CBOR.TListI xs) -> Just (CBOR.TTagged 258 . CBOR.TListI, xs)
      CBOR.TList xs -> Just (CBOR.TList, xs)
      CBOR.TListI xs -> Just (CBOR.TListI, xs)
      _ -> Nothing
    sized n bs = BS.take (fromInteger n) (bs <> BS.replicate (fromInteger n) 0)
