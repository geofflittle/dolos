{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Values generated with each rule of a mutable shape marked, and the CBOR
-- writer that drops the marks, changing one marked value on request.
module Mutation (
  Shape (..),
  Form (..),
  Occurrence (..),
  Change (..),
  marked,
  mark,
  markOf,
  write,
  changes,
  kinds,
  reachable,
) where

import Codec.CBOR.Cuddle.CBOR.Gen (generateFromName)
import Codec.CBOR.Cuddle.CDDL (Name (..), RangeBound (..), Value (..), ValueVariant (..))
import Codec.CBOR.Cuddle.CDDL.CTree (CTree (..), CTreeRoot (..), PTerm (..))
import Codec.CBOR.Cuddle.CDDL.Custom.Core (RuleTerm (..))
import Codec.CBOR.Cuddle.CDDL.Resolve (MonoReferenced, XXCTree (..))
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Term as CBOR
import qualified Codec.CBOR.Write as CBOR
import qualified Data.ByteString as BS
import qualified Data.ByteString.Builder as B
import qualified Data.ByteString.Lazy as BSL
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as T
import Data.Word (Word64, Word8)

-- | The shape of a rule whose values a change applies to.
data Shape
  = -- | A choice whose arms each start with a distinct unsigned literal, and whether every arm is an array.
    Sum [Word64] Bool
  | -- | A set or ordered set of elements.
    SetOf
  | -- | A rational number under tag 30.
    Rational
  | -- | An array of fields, or a choice of such arrays, and whether a field is a boolean.
    ArrayRecord Bool
  | -- | A map of fields, and its keys when every key is an unsigned literal.
    MapRecord [Word64]
  | -- | An integer from a lowest to a highest value.
    Bounded Integer Integer
  deriving (Eq, Show)

-- | How the writer writes the heads of a value.
data Form
  = -- | Each head in its shortest form and each array and map with its length.
    Shortest
  | -- | Each integer and length in eight bytes.
    WideHeads
  | -- | Each array and map of indefinite length.
    IndefiniteLengths
  deriving (Eq, Show)

-- | The kinds of change every rule of the shape receives.
kinds :: Shape -> [T.Text]
kinds = \case
  Sum _ arrays -> "unknown-arm" : if arrays then arrayKinds else []
  SetOf -> ["set-duplicate", "set-two-ways", "set-reorder", "set-empty"]
  Rational -> ["rational-tag", "rational-third", "rational-unreduced"]
  ArrayRecord flips -> arrayKinds <> ["bool-flip" | flips]
  MapRecord ks -> ["unknown-key" | not (null ks)]
  Bounded lo hi -> ["range-below" | lo - 1 >= lowest] <> ["range-above" | hi + 1 <= highest]
  where
    arrayKinds = ["array-extra", "array-short", "array-empty"]

-- | The lowest and the highest integer a CBOR head holds.
lowest, highest :: Integer
lowest = negate (2 ^ (64 :: Int))
highest = 2 ^ (64 :: Int) - 1

-- | One marked value of a written term.
data Occurrence = Occurrence
  { occNumber :: Int
  , occRule :: Name
  , occShape :: Shape
  , occSite :: T.Text
  , occTerm :: CBOR.Term
  }

-- | A change to one marked value, named by its kind and a detail.
data Change = Change {changeKind :: T.Text, changeDetail :: T.Text, changeApply :: CBOR.Term -> CBOR.Term}

-- | The tag of the first mark, above every tag the spec defines.
markBase :: Word64
markBase = 0xFFFFFFFF00000000

-- | The tag under which the writer writes every integer and length in eight bytes.
wideTag :: Word64
wideTag = markBase - 1

-- | The shape of a rule, if a change applies to its values.
shapeOf :: Map.Map Name (CTree MonoReferenced) -> Name -> CTree MonoReferenced -> Maybe Shape
shapeOf m n t
  | set n = Just SetOf
  | CTreeE (MGenerator _ x) <- t, CTreeE (MRuleRef r) <- unwrap x, set r = Just SetOf
  | otherwise = case unwrap t of
      Tag 30 _ -> Just Rational
      Choice arms
        | Just ks <- traverse (leading m) (NE.toList arms) -> Just (Sum ks (all array (NE.toList arms)))
        | Just fields <- traverse record (NE.toList arms) -> Just (ArrayRecord (any (any (boolean m)) fields))
      Map xs -> Just (MapRecord (keys xs))
      Tag _ x | Map xs <- unwrap x -> Just (MapRecord (keys xs))
      Array xs | Just fields <- record (Array xs) -> Just (ArrayRecord (any (boolean m) fields))
      Range lower upper bound
        | Just lo <- integerOf m lower
        , Just hi <- integerOf m upper ->
            Just (Bounded lo (if bound == Closed then hi else hi - 1))
      _ -> Nothing
  where
    set r = any (`T.isPrefixOf` unName r) ["%set<", "%nonempty_set<", "%oset<", "%nonempty_oset<"]
    repeated = \case
      Occur {} -> True
      _ -> False
    array = \case
      Array _ -> True
      _ -> False
    record = \case
      Array xs | not (any repeated xs) -> Just xs
      _ -> Nothing
    keys xs = fromMaybe [] (traverse literalKey xs)
    literalKey = \case
      Occur x _ -> literalKey x
      KV k _ _ | Literal (Value (VUInt u) _) <- unwrap k -> Just u
      _ -> Nothing

-- | Whether an item of an array is a boolean.
boolean :: Map.Map Name (CTree MonoReferenced) -> CTree MonoReferenced -> Bool
boolean m = go (8 :: Int)
  where
    go 0 _ = False
    go d t = case unwrap t of
      Literal (Value (VBool _) _) -> True
      Postlude PTBool -> True
      KV _ v _ -> go (d - 1) v
      CTreeE (MRuleRef r) -> maybe False (go (d - 1)) (Map.lookup r m)
      _ -> False

-- | The integer a bound of a range names, through rule references.
integerOf :: Map.Map Name (CTree MonoReferenced) -> CTree MonoReferenced -> Maybe Integer
integerOf m = go (8 :: Int)
  where
    go 0 _ = Nothing
    go d t = case unwrap t of
      Literal (Value (VUInt k) _) -> Just (toInteger k)
      Literal (Value (VNInt k) _) -> Just (negate (toInteger k))
      Literal (Value (VBignum k) _) -> Just k
      CTreeE (MRuleRef r) -> Map.lookup r m >>= go (d - 1)
      _ -> Nothing

unwrap :: CTree MonoReferenced -> CTree MonoReferenced
unwrap = \case
  CTreeE (MGenerator _ x) -> unwrap x
  CTreeE (MValidator _ x) -> unwrap x
  x -> x

-- | The unsigned literal an arm of a choice starts with, when the arm is one value.
leading :: Map.Map Name (CTree MonoReferenced) -> CTree MonoReferenced -> Maybe Word64
leading m = \case
  Literal (Value (VUInt k) _) -> Just k
  Array (x : _) -> first (8 :: Int) x
  _ -> Nothing
  where
    first 0 = const Nothing
    first d = \case
      Literal (Value (VUInt k) _) -> Just k
      Group (x : _) -> first (d - 1) x
      CTreeE (MRuleRef r) -> Map.lookup r m >>= first (d - 1)
      _ -> Nothing

-- | The spec with each rule of a shape, other than the excluded ones,
-- generating its values under a mark naming it, and the rules of a shape,
-- each with its shape.
marked :: Set.Set Name -> CTreeRoot MonoReferenced -> (CTreeRoot MonoReferenced, [(Name, Shape)])
marked excluded (CTreeRoot m) = (CTreeRoot (Map.unions [wrapped, own, m]), table)
  where
    table = [(n, s) | (n, t) <- Map.toList m, Just s <- [shapeOf m n t]]
    kept = [(i, n) | (i, (n, _)) <- zip [0 ..] table, Set.notMember n excluded]
    own = Map.fromList [(ownName n, m Map.! n) | (_, n) <- kept]
    wrapped =
      Map.fromList
        [ (n, CTreeE (MGenerator (SingleTerm . CBOR.TTagged (markBase + i) <$> generateFromName (ownName n)) (m Map.! n)))
        | (i, n) <- kept
        ]
    ownName n = Name (unName n <> "!")

-- | The tag of the mark of a rule of the table.
markOf :: [(Name, Shape)] -> Name -> Maybe Word64
markOf table n = (\i -> markBase + fromIntegral i) <$> lookup n (zip (map fst table) [0 :: Int ..])

-- | A value under the mark of a rule of the table, or unmarked when the rule has no shape.
mark :: [(Name, Shape)] -> Name -> CBOR.Term -> CBOR.Term
mark table n t = maybe t (`CBOR.TTagged` t) (markOf table n)

-- | Where the writer is: the enclosing record and the field in it, how it
-- writes integers and whether it writes lengths.
data Place = Place {placeRecord :: Maybe (Name, Maybe T.Text), placeWide :: Bool, placeIndefinite :: Bool}

-- | Writes a marked term in a form without its marks, applying the change to
-- the occurrence numbered `target`, and returns the occurrences it passed.
write :: [(Name, Shape)] -> Maybe (Int, CBOR.Term -> CBOR.Term) -> Form -> CBOR.Term -> (BS.ByteString, [Occurrence])
write table wanted form t =
  let (b, _, occs) = go (Place Nothing (form == WideHeads) (form == IndefiniteLengths)) 0 t
   in (BSL.toStrict (B.toLazyByteString b), occs)
  where
    go :: Place -> Int -> CBOR.Term -> (B.Builder, Int, [Occurrence])
    go p n = \case
      CBOR.TTagged tag x
        | tag == wideTag -> go p {placeWide = True} n x
        | tag >= markBase, Just (rule, shape) <- lookupMark tag ->
            let occ = Occurrence n rule shape (site p) x
                x' = case wanted of
                  Just (k, f) | k == n -> f x
                  _ -> x
                p' = if opensPlace shape then p {placeRecord = Just (rule, Nothing)} else p
                (b, n', occs) = go p' (n + 1) x'
             in (b, n', occ : occs)
        | otherwise -> cat (header p 6 tag) (go p n x)
      CBOR.TList xs | not (placeIndefinite p) -> items p n (header p 4 (fromIntegral (length xs))) (zip (map (T.pack . show) [0 :: Int ..]) xs) mempty
      CBOR.TList xs -> items p n (B.word8 0x9f) (zip (map (T.pack . show) [0 :: Int ..]) xs) (B.word8 0xff)
      CBOR.TListI xs -> items p n (B.word8 0x9f) (zip (map (T.pack . show) [0 :: Int ..]) xs) (B.word8 0xff)
      CBOR.TMap kvs | not (placeIndefinite p) -> pairs p n (header p 5 (fromIntegral (length kvs))) kvs mempty
      CBOR.TMap kvs -> pairs p n (B.word8 0xbf) kvs (B.word8 0xff)
      CBOR.TMapI kvs -> pairs p n (B.word8 0xbf) kvs (B.word8 0xff)
      CBOR.TBytes bs | Just nested <- embedded bs ->
        let (b, n', occs) = go p n nested
            bytes = BSL.toStrict (B.toLazyByteString b)
         in (header p 2 (fromIntegral (BS.length bytes)) <> B.byteString bytes, n', occs)
      CBOR.TBytes bs | placeWide p -> (header p 2 (fromIntegral (BS.length bs)) <> B.byteString bs, n, [])
      CBOR.TString s | placeWide p -> let bs = T.encodeUtf8 s in (header p 3 (fromIntegral (BS.length bs)) <> B.byteString bs, n, [])
      CBOR.TInt i | placeWide p -> (integer p (toInteger i), n, [])
      CBOR.TInteger i | placeWide p, i >= negate (2 ^ (64 :: Int)), i < 2 ^ (64 :: Int) -> (integer p i, n, [])
      leaf -> (CBOR.toBuilder (CBOR.encodeTerm leaf), n, [])
    cat b (b', n, occs) = (b <> b', n, occs)
    items p n open xs close =
      let step (acc, k, os) (pos, x) = let (b', k', os') = go (field p pos) k x in (acc <> b', k', os <> os')
          (b, n', occs) = foldl step (open, n, []) xs
       in (b <> close, n', occs)
    pairs p n open kvs close =
      let step (acc, k, os) (kt, v) =
            let (bk, k1, ok) = go (field p "key") k kt
                (bv, k2, ov) = go (field p (keyName kt)) k1 v
             in (acc <> bk <> bv, k2, os <> ok <> ov)
          (b, n', occs) = foldl step (open, n, []) kvs
       in (b <> close, n', occs)
    field p pos = case placeRecord p of
      Just (r, Nothing) -> p {placeRecord = Just (r, Just pos)}
      _ -> p
    keyName = \case
      CBOR.TInt i -> T.pack (show i)
      CBOR.TString s -> s
      _ -> "*"
    site p = case placeRecord p of
      Just (r, Just pos) -> unName r <> " " <> pos
      Just (r, Nothing) -> unName r
      Nothing -> ""
    lookupMark tag = let i = fromIntegral (tag - markBase) in if i < length table then Just (table !! i) else Nothing
    opensPlace = \case
      SetOf -> False
      Rational -> False
      Bounded {} -> False
      _ -> True
    embedded bs = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict bs) of
      Right (rest, nested) | BSL.null rest, hasMark nested -> Just nested
      _ -> Nothing
    hasMark = \case
      CBOR.TTagged tag x -> tag >= wideTag || hasMark x
      CBOR.TList xs -> any hasMark xs
      CBOR.TListI xs -> any hasMark xs
      CBOR.TMap kvs -> any (\(k, v) -> hasMark k || hasMark v) kvs
      CBOR.TMapI kvs -> any (\(k, v) -> hasMark k || hasMark v) kvs
      _ -> False

-- | The head of a data item, in eight bytes when the place writes wide.
header :: Place -> Word8 -> Word64 -> B.Builder
header p major v
  | placeWide p = B.word8 (major * 32 + 27) <> B.word64BE v
  | v < 24 = B.word8 (major * 32 + fromIntegral v)
  | v < 0x100 = B.word8 (major * 32 + 24) <> B.word8 (fromIntegral v)
  | v < 0x10000 = B.word8 (major * 32 + 25) <> B.word16BE (fromIntegral v)
  | v < 0x100000000 = B.word8 (major * 32 + 26) <> B.word32BE (fromIntegral v)
  | otherwise = B.word8 (major * 32 + 27) <> B.word64BE v

-- | An integer of the range of a head, in eight bytes.
integer :: Place -> Integer -> B.Builder
integer p i
  | i >= 0 = header p 0 (fromInteger i)
  | otherwise = header p 1 (fromInteger (negate 1 - i))

-- | The rules a value of any of the named rules can contain, those included.
reachable :: CTreeRoot MonoReferenced -> [Name] -> Set.Set Name
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
      CTreeE (MRuleRef r) -> [r]
      CTreeE (MGenerator _ x) -> refs x
      CTreeE (MValidator _ x) -> refs x

-- | The changes that apply to a value of a shape.
changes :: Shape -> CBOR.Term -> [Change]
changes shape t = case shape of
  Sum ks _ -> unknownArms ks <> arrays
  ArrayRecord _ -> arrays <> flips
  MapRecord ks ->
    [ Change "unknown-key" (T.pack (show u)) (addKey u)
    | not (null ks)
    , u <- [k | k <- [0 .. maximum ks + 1], k `notElem` ks]
    ]
  Bounded lo hi ->
    [Change "range-below" (T.pack (show (lo - 1))) (const (CBOR.TInteger (lo - 1))) | lo - 1 >= lowest]
      <> [Change "range-above" (T.pack (show (hi + 1))) (const (CBOR.TInteger (hi + 1))) | hi + 1 <= highest]
  SetOf -> case elementsOf t of
    Just (wrap, xs@(x : _)) ->
      [Change "set-duplicate" "" (const (wrap (xs <> [x])))]
        <> [Change "set-two-ways" "" (const (wrap (xs <> [CBOR.TTagged wideTag x])))]
        <> [Change "set-reorder" "" (const (wrap (reverse xs))) | reverse xs /= xs]
        <> [Change "set-empty" "" (const (wrap []))]
    _ -> []
  Rational -> case t of
    CBOR.TTagged 30 (CBOR.TList [n, d]) ->
      [ Change "rational-tag" "31" (const (CBOR.TTagged 31 (CBOR.TList [n, d])))
      , Change "rational-third" "" (const (CBOR.TTagged 30 (CBOR.TList [n, d, CBOR.TInt 1])))
      ]
        <> [ Change "rational-unreduced" "" (const (CBOR.TTagged 30 (CBOR.TList [CBOR.TInteger (2 * a), CBOR.TInteger (2 * b)])))
           | Just a <- [intOf n]
           , Just b <- [intOf d]
           , 2 * max a b < 2 ^ (64 :: Int)
           ]
    _ -> []
  where
    unknownArms ks =
      [ Change "unknown-arm" (T.pack (show u)) (setLeading u)
      | Just _ <- [leadingOf t]
      , u <- [k | k <- [0 .. maximum ks + 1], k `notElem` ks]
      ]
    arrays = case t of
      CBOR.TList xs@(_ : _) ->
        [ Change "array-extra" "" (const (CBOR.TList (xs <> [CBOR.TInt 0])))
        , Change "array-short" "" (const (CBOR.TList (init xs)))
        , Change "array-empty" "" (const (CBOR.TList []))
        ]
      _ -> []
    flips = case t of
      CBOR.TList xs ->
        [ Change "bool-flip" (T.pack (show (length before))) (const (CBOR.TList (before <> [CBOR.TBool (not b)] <> after)))
        | (before, CBOR.TBool b : after) <- [splitAt i xs | i <- [0 .. length xs - 1]]
        ]
      _ -> []
    addKey u = \case
      CBOR.TMap kvs -> CBOR.TMap (kvs <> [(CBOR.TInteger (toInteger u), CBOR.TInt 0)])
      CBOR.TMapI kvs -> CBOR.TMapI (kvs <> [(CBOR.TInteger (toInteger u), CBOR.TInt 0)])
      CBOR.TTagged tag x -> CBOR.TTagged tag (addKey u x)
      x -> x
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
      CBOR.TTagged tag x | tag >= markBase -> intOf x
      _ -> Nothing
    elementsOf = \case
      CBOR.TTagged 258 (CBOR.TList xs) -> Just (CBOR.TTagged 258 . CBOR.TList, xs)
      CBOR.TTagged 258 (CBOR.TListI xs) -> Just (CBOR.TTagged 258 . CBOR.TListI, xs)
      CBOR.TList xs -> Just (CBOR.TList, xs)
      CBOR.TListI xs -> Just (CBOR.TListI, xs)
      _ -> Nothing
