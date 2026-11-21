{-# LANGUAGE GADTs #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}

-- | The nodes of a value the spec accepts, each with its path from the rule the value is read as.
module Path (
  Node (..),
  Reading (..),
  Step (..),
  Part (..),
  Tree,
  nodes,
  stepJSON,
  stepText,
  textRule,
) where

import Codec.CBOR.Cuddle.CBOR.Validator (validateCBOR)
import Codec.CBOR.Cuddle.CBOR.Validator.Trace (
  ControlInfo (..),
  Evidenced (..),
  ListValidationTrace (..),
  MapValidationTrace (..),
  SValidity (..),
  ValidationTrace (..),
 )
import Codec.CBOR.Cuddle.CDDL (Name (..), OccurrenceIndicator (..), Value (..), ValueVariant (..))
import Codec.CBOR.Cuddle.CDDL.CTree (CTree (..), CTreeRoot (..))
import Codec.CBOR.Cuddle.CDDL.CtlOp (CtlOp (Cbor))
import Codec.CBOR.Cuddle.CDDL.Custom.Validator (ValidatorPhase, XXCTree (..))
import Codec.CBOR.Cuddle.CDDL.Resolve (MonoSimplePhase, XXCTree (..))
import Codec.CBOR.Cuddle.IndexMappable (mapIndex)
import qualified Codec.CBOR.Read as CBOR
import qualified Codec.CBOR.Term as CBOR
import Data.Aeson ((.=))
import qualified Data.Aeson as Aeson
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BSL
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import Data.Word (Word64)

-- | One step from a rule to a node beneath it, each rule named as the CDDL text names it.
data Step
  = Key T.Text Word64
  | Member T.Text Part
  | Arm T.Text Int
  | Element
  | EntryKey
  | EntryValue
  deriving (Eq, Ord, Show)

-- | A declared member of an array, by its name, or by its position when the rule gives none.
data Part = Named T.Text | Position Int
  deriving (Eq, Ord, Show)

-- | A node of a value: its position in the term, each tree it is read as, whether it is bytes the spec reads as CBOR, whether it is an item of a repeated member or a key or value of a table, and its term.
data Node = Node
  { nodePosition :: [Int]
  , nodeReadings :: [Reading]
  , nodeCbor :: Bool
  , nodeElement :: Bool
  , nodeTerm :: CBOR.Term
  }

-- | A tree a node is read as, the rule it defines if any, and the path at which it is read.
data Reading = Reading
  { readingRule :: Maybe Name
  , readingTree :: Tree
  , readingPath :: [Step]
  }

-- | Where the walk is: the position and the path, both reversed, the rules on the path, whether the path has stopped, whether the node is an item of a repeated member or a key or value of a table, and the rule whose definition holds the node.
data At = At
  { atPosition :: [Int]
  , atPath :: [Step]
  , atOn :: Set.Set Name
  , atStopped :: Bool
  , atElement :: Bool
  , atOwner :: Name
  }

-- | The spec as the validator reads it and as its traces name it.
data Spec = Spec
  { specValidating :: CTreeRoot ValidatorPhase
  , specSimple :: CTreeRoot MonoSimplePhase
  }

type Tree = CTree MonoSimplePhase

-- | The rule of the CDDL text a rule of the spec instantiates, `set` for `%set<transaction_input>`.
textRule :: Name -> T.Text
textRule = T.takeWhile (/= '<') . T.dropWhile (== '%') . unName

-- | A step as a record's place writes it.
stepJSON :: Step -> Aeson.Value
stepJSON = \case
  Key r k -> Aeson.object ["key" .= Aeson.object ["rule" .= r, "key" .= k]]
  Member r p -> Aeson.object ["member" .= Aeson.object ["rule" .= r, "member" .= part p]]
  Arm r i -> Aeson.object ["arm" .= Aeson.object ["rule" .= r, "arm" .= i]]
  Element -> "element"
  EntryKey -> "entry_key"
  EntryValue -> "entry_value"
  where
    part = \case
      Named t -> Aeson.toJSON t
      Position i -> Aeson.toJSON i

-- | A step as a record's site writes it.
stepText :: Step -> T.Text
stepText = \case
  Key r k -> r <> " " <> T.pack (show k)
  Member r (Named t) -> r <> " " <> t
  Member r (Position i) -> r <> " " <> T.pack (show i)
  Arm r i -> r <> " arm " <> T.pack (show i)
  Element -> "element"
  EntryKey -> "entry key"
  EntryValue -> "entry value"

-- | The nodes of bytes the spec reads as the rule, or none when it does not.
nodes :: CTreeRoot ValidatorPhase -> Name -> BS.ByteString -> Maybe [Node]
nodes root n bs = traced spec n bs (walk spec (At [] [] Set.empty False False n) [] (CTreeE (MSimpleRef n)))
  where
    spec = Spec root (mapIndex root)

-- | The term of bytes the spec reads as the rule, given with the trace of the reading.
traced :: Spec -> Name -> BS.ByteString -> (forall v. CBOR.Term -> ValidationTrace v -> a) -> Maybe a
traced spec n bs k = case (decoded bs, validateCBOR bs top (CTreeRoot (Map.insert top (CTreeE (VRuleRef n)) m))) of
  (Just term, Right (Evidenced SValid t)) -> Just (k term t)
  _ -> Nothing
  where
    CTreeRoot m = specValidating spec
    top = Name "%value"

decoded :: BS.ByteString -> Maybe CBOR.Term
decoded bs = case CBOR.deserialiseFromBytes CBOR.decodeTerm (BSL.fromStrict bs) of
  Right (rest, t) | BSL.null rest -> Just t
  _ -> Nothing

-- | The nodes of a term from the trace of the spec reading it, given the readings of the node so far and the tree the trace reads.
walk :: Spec -> At -> [Reading] -> Tree -> CBOR.Term -> ValidationTrace v -> [Node]
walk spec at here tree term = \case
  ReferenceRule r t ->
    let at' = at {atOn = Set.insert r (atOn at), atStopped = atStopped at || Set.member r (atOn at), atOwner = r}
        tree' = Map.findWithDefault tree r rules
     in walk spec at' (here <> [Reading (Just r) tree' (reverse (atPath at))]) tree' term t
  ChoiceBranch i t -> case tree of
    Choice opts | x : _ <- drop i (NE.toList opts) -> walk spec (stepped (Arm (textRule (atOwner at)) i) at) here x term t
    _ -> walk spec at here tree term t
  ControlTrace info t -> case (ciOp info, ciRule info, term) of
    (Cbor, CTreeE (MSimpleRef r), CBOR.TBytes b) ->
      node True : maybe [] id (traced spec r b (walk spec (child 0 at) [] (CTreeE (MSimpleRef r))))
    _ -> walk spec at here (case tree of Control _ x _ -> x; x -> x) term t
  TagTrace _ t -> case term of
    CBOR.TTagged _ x -> node False : walk spec (child 0 at) [] (case tree of Tag _ y -> y; y -> y) x t
    _ -> [node False]
  ListTrace lt -> node False : fst (items spec at (case tree of Array ds -> ds; _ -> []) 0 0 (elements term) lt)
  MapTrace mt -> node False : entries spec at (pairs term) 0 mt
  _ -> [node False]
  where
    CTreeRoot rules = specSimple spec
    node cbor = Node (reverse (atPosition at)) here cbor (atElement at) term
    elements = \case
      CBOR.TList xs -> xs
      CBOR.TListI xs -> xs
      _ -> []
    pairs = \case
      CBOR.TMap kvs -> kvs
      CBOR.TMapI kvs -> kvs
      _ -> []

-- | The nodes of the items of a list the trace consumes, given the declared members of the array or group the owner of the place defines, the cursor over them and the index of the next item, with the index after the last item consumed.
items :: Spec -> At -> [Tree] -> Int -> Int -> [CBOR.Term] -> ListValidationTrace v -> ([Node], Int)
items spec at ds j k xs = \case
  ListValidationConsume e t rest ->
    let (s, j') = member (textRule (atOwner at)) ds j e
        here = case drop k xs of
          x : _ -> inline spec (stepped s (child k at)) (entry e) x t
          [] -> []
        (more, k') = items spec at ds j' (k + 1) xs rest
     in (here <> more, k')
  ListValidationConsumeGroup names gt rest ->
    let (gat, gds, j') = case names of
          [] -> case [(i, g) | (i, d) <- drop j (zip [0 ..] ds), Just g <- [groupOf d]] of
            (i, g) : _ -> (at, g, next i)
            [] -> (at, [], j)
          first : _ ->
            let g = last names
                gds' = case Map.lookup g rules of
                  Just (Group xs') -> xs'
                  _ -> []
                on = foldr Set.insert (atOn at) names
                at' = at {atOn = on, atStopped = atStopped at || any (`Set.member` atOn at) names, atOwner = g}
             in case [i | (i, d) <- drop j (zip [0 ..] ds), refers first d] of
                  i : _ -> (at', gds', next i)
                  [] -> (at', gds', j)
        (inner, k1) = items spec gat gds 0 k xs gt
        (more, k2) = items spec at ds j' k1 xs rest
     in (inner <> more, k2)
  _ -> ([], k)
  where
    CTreeRoot rules = specSimple spec
    groupOf = \case
      Group g -> Just g
      Occur (Group g) _ -> Just g
      _ -> Nothing
    refers r = \case
      CTreeE (MSimpleRef r') -> r == r'
      Occur x _ -> refers r x
      KV _ v _ -> refers r v
      _ -> False
    next i = if repeated (ds !! i) then i else i + 1

-- | The step to the declared member of an array or group an entry consumes, and the cursor after it.
member :: T.Text -> [Tree] -> Int -> Tree -> (Step, Int)
member owner ds j e
  | repeated e = (Element, maybe j fst found)
  | otherwise = case found of
      Just (i, d) -> (Member owner (maybe (Position i) Named (named d)), i + 1)
      Nothing -> (Member owner (Position j), j + 1)
  where
    found = case [(i, d) | (i, d) <- drop j (zip [0 ..] ds), d == e] of
      x : _ -> Just x
      [] -> Nothing
    named = \case
      Occur x OIOptional -> named x
      KV (Literal (Value (VText t) _)) _ _ -> Just t
      _ -> Nothing

-- | The nodes of the entries of a map the trace consumes, in the order of the term.
entries :: Spec -> At -> [(CBOR.Term, CBOR.Term)] -> Int -> MapValidationTrace v -> [Node]
entries spec at kvs k = \case
  MapValidationConsume e kt vt rest ->
    let here = case (drop k kvs, literalKey e) of
          ((_, vx) : _, Just u) -> inline spec (stepped (Key (textRule (atOwner at)) u) (child (2 * k + 1) at)) (entry e) vx vt
          ((kx, vx) : _, Nothing) ->
            inline spec (stepped EntryKey (child (2 * k) at)) (keyTree e) kx kt
              <> inline spec (stepped EntryValue (child (2 * k + 1) at)) (entry e) vx vt
          ([], _) -> []
     in here <> entries spec at kvs (k + 1) rest
  _ -> []
  where
    literalKey = \case
      Occur x OIOptional -> literalKey x
      KV (Literal (Value (VUInt u) _)) _ _ -> Just u
      _ -> Nothing
    keyTree = \case
      Occur x _ -> keyTree x
      KV kt' _ _ -> kt'
      x -> x

-- | The nodes of an item or entry declared as a tree, which the node is read as when it names no rule.
inline :: Spec -> At -> Tree -> CBOR.Term -> ValidationTrace v -> [Node]
inline spec at tree = walk spec at readings tree
  where
    readings = case tree of
      CTreeE _ -> []
      _ -> [Reading Nothing tree (reverse (atPath at))]

-- | Whether a declared member repeats, so that each item it consumes is an element.
repeated :: Tree -> Bool
repeated = \case
  Occur _ OIOptional -> False
  Occur _ _ -> True
  _ -> False

-- | The tree an item of a declared member is read as.
entry :: Tree -> Tree
entry = \case
  Occur x _ -> entry x
  KV _ v _ -> v
  x -> x

-- | The place one step further, unless the path has stopped, under an item or entry from a step to one.
stepped :: Step -> At -> At
stepped s at = (if atStopped at then at else at {atPath = s : atPath at}) {atElement = atElement at || s `elem` [Element, EntryKey, EntryValue]}

-- | The place at the child of the node at a position among its children.
child :: Int -> At -> At
child k at = at {atPosition = k : atPosition at, atElement = False}
