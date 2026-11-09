{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Mutants of each accepted transaction, one per operator at a site the seed picks, written with the ledger's verdict on the changed bytes.
module Mutate (Mutants (..), Operator (..), operators, withMutants, capturingKeys, recordTo) where

import Cardano.Crypto.Hash (Blake2b_256, hashToBytes, hashWith)
import Cardano.Ledger.BaseTypes (Globals (..), pvMajor)
import Cardano.Ledger.Dijkstra.Core
import Cardano.Ledger.Keys (KeyHash, KeyRole (Witness))
import Cardano.Ledger.Shelley.API.Mempool (ApplyTx (mkStAnnTx))
import Cardano.Ledger.Shelley.LedgerState (lsUTxOStateL)
import Cardano.Ledger.Shelley.Rules (LedgerEnv (..))
import Cardano.Ledger.State (UTxO (..), utxoL)
import Cardano.Ledger.TxIn (TxIn)
import Cardano.Slotting.Slot (SlotNo (..))
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term (..), decodeTerm)
import Control.Monad (forM_, (>=>))
import Control.Monad.IO.Class (liftIO)
import Control.Monad.Reader (runReader)
import Control.State.Transition.Extended (STS (..), TRC (..), applySTS)
import qualified Data.Aeson as A
import qualified Data.ByteString as BS
import qualified Data.ByteString.Char8 as BS8
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Lazy.Char8 as BL8
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import Data.List (foldl', intercalate)
import qualified Data.List.NonEmpty as NE
import qualified Data.Map.Strict as Map
import qualified Data.OMap.Strict as OMap
import qualified Data.Set as Set
import Data.Word (Word64)
import Dump (E, Line (..), decodeTx, dumpRecord, recordLine)
import Edit
import Lens.Micro ((%~), (&), (^.))
import Lens.Micro.Mtl (use)
import System.Environment (getArgs, withArgs)
import System.IO.Unsafe (unsafePerformIO)
import Test.Cardano.Ledger.Core.KeyPair (KeyPair)
import Test.Cardano.Ledger.Dijkstra.ImpTest
import Test.ImpSpec (ImpInit (..), SpecWith, modifyImpInit)

-- | The seed, the corpus name and the first mutant number of one writer's mutants.
data Mutants = Mutants
  { mutantsSeed :: String
  , mutantsCorpus :: String
  , mutantsBase :: Int
  }

-- | A named change with the sites of a transaction term where it applies at a slot, each with its edit.
data Operator = Operator
  { opName :: String
  , opSites :: Int -> Term -> [(String, Edit)]
  }

-- | Runs a writer with the mutants that a leading `--mutants <seed>` argument asks for.
withMutants :: String -> Int -> (Maybe Mutants -> IO ()) -> IO ()
withMutants corpus base run = do
  args <- getArgs
  case args of
    "--mutants" : seed : rest -> withArgs rest (run (Just (Mutants seed corpus base)))
    _ -> run Nothing

{-# NOINLINE capturedKeys #-}
-- | The key pairs of the Imp state at the last fixup.
capturedKeys :: IORef (Map.Map (KeyHash Witness) (KeyPair Witness))
capturedKeys = unsafePerformIO (newIORef Map.empty)

-- | The spec with a fixup that saves the key pairs the post submit hook signs mutants with.
capturingKeys :: SpecWith (ImpInit (LedgerSpec E)) -> SpecWith (ImpInit (LedgerSpec E))
capturingKeys = modifyImpInit $ \i -> i {impInitEnv = impInitEnv i & iteFixupL %~ (>=> save)}
  where
    save tx = do
      keys <- use impKeyPairsG
      liftIO (writeIORef capturedKeys keys)
      pure tx

-- | Writes the record of one LEDGER rule run and, when the ledger accepted it, its mutants.
recordTo ::
  Maybe Mutants ->
  FilePath ->
  Globals ->
  TRC (EraRule "LEDGER" E) ->
  Either (NE.NonEmpty (PredicateFailure (EraRule "LEDGER" E))) (State (EraRule "LEDGER" E), [Event (EraRule "LEDGER" E)]) ->
  ImpM t ()
recordTo mutants out g trc res = do
  parent <- dumpRecord out g trc res
  case (mutants, parent) of
    (Just m, Just (n, bytes)) -> liftIO (writeMutants m out g trc (fmap fst res) n bytes)
    _ -> pure ()

-- | Appends one mutant per operator with a site to the writer's mutant file, and each mutant that cannot be built to its skip file.
writeMutants ::
  Mutants ->
  FilePath ->
  Globals ->
  TRC (EraRule "LEDGER" E) ->
  Either (NE.NonEmpty (PredicateFailure (EraRule "LEDGER" E))) (State (EraRule "LEDGER" E)) ->
  Int ->
  BS.ByteString ->
  IO ()
writeMutants m out g (TRC (env, st, stAnn)) parentRes n bytes = do
  keys <- readIORef capturedKeys
  let tx = stAnn ^. txStAnnTxG
      pp = ledgerPp env
      SlotNo slot = ledgerSlotNo env
      term = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
        Right (rest, t) | BL.null rest -> t
        _ -> error "accepted bytes that are not one CBOR term"
      parent = mutantsCorpus m <> "#" <> show n
      skip op why = BL8.appendFile (out <> ".skipped") (BL8.pack (intercalate "\t" [op, parent, why]) <> "\n")
      narrowed t = st & lsUTxOStateL . utxoL %~ \(UTxO u) -> UTxO (Map.restrictKeys u (named t))
  forM_ (zip [0 ..] operators) $ \(ix, op) -> case opSites op (fromIntegral slot) term of
    [] -> pure ()
    sites -> do
      let r = rank m n (opName op)
          (site, edit) = sites !! fromIntegral (r `mod` fromIntegral (length sites))
          number = mutantsBase m + n * length operators + ix
      case editBytes pp (`Map.lookup` keys) edit tx term of
        Left why -> skip (opName op) why
        Right mutated -> do
          let (trcM, resM) = case decodeTx (pvMajor (pp ^. ppProtocolVersionL)) mutated of
                Right txM ->
                  let stM = narrowed txM
                      annotated = mkStAnnTx (epochInfo g) (systemStart g) pp (stM ^. lsUTxOStateL . utxoL) mempty txM
                      trcA = TRC (env, stM, annotated)
                   in (trcA, runReader (applySTS @(EraRule "LEDGER" E) trcA) g)
                Left _ -> (TRC (env, narrowed tx, stAnn), parentRes)
              Line {lineValue, lineRule} = recordLine number Nothing (Just mutated) g trcM resM
          case lineRule of
            Left why -> skip (opName op) why
            Right _ ->
              BL8.appendFile (out <> ".mutants") $
                BL8.pack (opName op <> "\t" <> show r <> "\t")
                  <> A.encode
                    ( A.object
                        [ "mutation" A..= A.object ["operator" A..= opName op, "parent" A..= parent, "site" A..= site]
                        , "record" A..= lineValue
                        ]
                    )
                  <> "\n"

-- | The inputs, collateral inputs and reference inputs a transaction names at its top level and in each of its sub transactions.
named :: Tx TopTx E -> Set.Set TxIn
named tx = Set.unions (tx ^. bodyTxL . allInputsTxBodyF : concat [[b ^. inputsTxBodyL, b ^. referenceInputsTxBodyL] | s <- OMap.elems (tx ^. bodyTxL . subTransactionsTxBodyL), let b = s ^. bodyTxL])

-- | The number the seed gives a record and an operator, from the first eight bytes of their Blake2b 256 digest.
rank :: Mutants -> Int -> String -> Word64
rank m n op =
  foldl' (\acc b -> acc * 256 + fromIntegral b) 0 $
    BS.unpack (BS.take 8 (hashToBytes (hashWith @Blake2b_256 id (BS8.pack (intercalate ":" [mutantsSeed m, mutantsCorpus m, show n, op])))))

-- | The operators, in the order that numbers their mutants.
operators :: [Operator]
operators =
  [ Operator "drop_witness" $ \_ t -> witnessSites t (\i -> overList (dropAt i))
  , Operator "duplicate_witness" $ \_ t -> witnessSites t (\i -> overList (dupAt i))
  , Operator "flip_guard" $ \_ t ->
      concat
        [ [ (name lvl <> " guard " <> show i, atBody lvl (overKey 14 (overList (overAt i flipCredential))))
          | (i, g) <- zip [0 ..] (maybe [] elems (lookupKey 14 b))
          , flipCredential g /= g
          ]
            <> [ (name lvl <> " required top level guard " <> show i, atBody lvl (overKey 24 (overEntries (overAt i (\(k, v) -> (flipCredential k, v))))))
               | (i, (k, _)) <- zip [0 ..] (maybe [] entries (lookupKey 24 b))
               , flipCredential k /= k
               ]
        | (lvl, b, _) <- levels t
        ]
  , Operator "remove_redeemer" $ \_ t ->
      [ (name lvl <> " redeemer " <> show i, atWits lvl (shrink 5 . overKey 5 (overList (dropAt i) . overEntries (dropAt i))))
      | (lvl, _, w) <- levels t
      , Just r <- [lookupKey 5 w]
      , i <- [0 .. length (elems r) + length (entries r) - 1]
      ]
  , Operator "change_sub_field" $ \_ t ->
      [ (name lvl <> " key " <> show k, atBody lvl (changeField k v))
      | (lvl@(Sub _), b, _) <- levels t
      , (TInt k, v) <- entries b
      , k `notElem` [0, 1]
      ]
  , Operator "reorder_set" $ \_ t ->
      concat
        [ [ (name lvl <> " body key " <> show k, atBody lvl (overKey k (overList reverse)))
          | k <- [0, 4, 13, 14, 18, 20]
          , Just s <- [lookupKey k b]
          , length (elems s) >= 2
          ]
            <> [ (name lvl <> " witness key " <> show k, atWits lvl (overKey k (overList reverse)))
               | k <- witnessKeys
               , Just s <- [lookupKey k w]
               , length (elems s) >= 2
               ]
        | (lvl, b, w) <- levels t
        ]
  , Operator "swap_reference_script" $ \_ t ->
      concat
        [ [ (name lvl <> " output " <> show i, atBody lvl (overKey 1 (overList (overAt i swapScript))))
          | (i, o) <- zip [0 ..] (maybe [] elems (lookupKey 1 b))
          , hasScript o
          ]
            <> [ (name lvl <> " collateral return", atBody lvl (overKey 16 swapScript))
               | Just o <- [lookupKey 16 b]
               , hasScript o
               ]
        | (lvl, b, _) <- levels t
        ]
  , Operator "move_validity_bound" $ \slot t -> case levels t of
      (Top, b, _) : _ -> case lookupKey 8 b of
        Just _ -> [("top validity start", atBody Top (setKey 8 (TInt (slot + 1))))]
        Nothing -> [("top time to live", atBody Top (setKey 3 (TInt slot)))]
      _ -> []
  ]
  where
    witnessSites t f =
      [ (name lvl <> " witness key " <> show k <> " element " <> show i, atWits lvl (shrink k . overKey k (f i)))
      | (lvl, _, w) <- levels t
      , k <- witnessKeys
      , Just s <- [lookupKey k w]
      , i <- [0 .. length (elems s) - 1]
      ]
    witnessKeys = [0, 1, 2, 3, 4, 6, 7, 8]
    name Top = "top"
    name (Sub j) = "sub " <> show j

-- | The level of a transaction an edit applies to.
data Level = Top | Sub Int

-- | The top level and each sub transaction of a transaction term, with its body and witness set.
levels :: Term -> [(Level, Term, Term)]
levels (TList (b : w : _)) =
  (Top, b, w) : [(Sub j, sb, sw) | (j, TList (sb : sw : _)) <- zip [0 ..] (maybe [] elems (lookupKey 23 b))]
levels _ = []

-- | The edit that changes the body of one level.
atBody :: Level -> (Term -> Term) -> Edit
atBody Top f = noEdit {editBody = f}
atBody (Sub j) f = noEdit {editSubBody = \i -> if i == j then f else id}

-- | The edit that changes the witness set of one level.
atWits :: Level -> (Term -> Term) -> Edit
atWits Top f = noEdit {editWits = f}
atWits (Sub j) f = noEdit {editSubWits = \i -> if i == j then f else id}

-- | The elements of a list or set term.
elems :: Term -> [Term]
elems (TTagged _ t) = elems t
elems (TList xs) = xs
elems (TListI xs) = xs
elems _ = []

-- | A list or set term with its elements changed.
overList :: ([Term] -> [Term]) -> Term -> Term
overList f (TTagged n t) = TTagged n (overList f t)
overList f (TList xs) = TList (f xs)
overList f (TListI xs) = TListI (f xs)
overList _ t = t

-- | A map term with its entries changed.
overEntries :: ([(Term, Term)] -> [(Term, Term)]) -> Term -> Term
overEntries f (TMap kvs) = TMap (f kvs)
overEntries f (TMapI kvs) = TMapI (f kvs)
overEntries _ t = t

-- | The map term without the key when its value holds no element or entry.
shrink :: Int -> Term -> Term
shrink k t = case lookupKey k t of
  Just v | null (elems v) && null (entries v) -> deleteKey k t
  _ -> t

-- | The list without its element at an index.
dropAt :: Int -> [a] -> [a]
dropAt i xs = take i xs <> drop (i + 1) xs

-- | The list with its element at an index given again after it.
dupAt :: Int -> [a] -> [a]
dupAt i xs = take (i + 1) xs <> drop i xs

-- | The list with its element at an index changed.
overAt :: Int -> (a -> a) -> [a] -> [a]
overAt i f xs = [if j == i then f x else x | (j, x) <- zip [0 ..] xs]

-- | A credential with its key and script kinds exchanged, and a key hash made a script credential.
flipCredential :: Term -> Term
flipCredential (TList [TInt 0, h]) = TList [TInt 1, h]
flipCredential (TList [TInt 1, h]) = TList [TInt 0, h]
flipCredential (TBytes h) = TList [TInt 1, TBytes h]
flipCredential t = t

-- | The body with an integer field one higher, or without the field when it holds anything else.
changeField :: Int -> Term -> Term -> Term
changeField k (TInt v) = setKey k (TInt (v + 1))
changeField k (TInteger v) = setKey k (TInteger (v + 1))
changeField k _ = deleteKey k

-- | Whether an output term is a map with a reference script.
hasScript :: Term -> Bool
hasScript o = any ((== TInt 3) . fst) (entries o)

-- | An output with its reference script replaced by an empty native script.
swapScript :: Term -> Term
swapScript o = setKey 3 (if lookupKey 3 o == Just (script 1) then script 2 else script 1) o
  where
    script c = TTagged 24 (TBytes (encoded (TList [TInt 0, TList [TInt c, TList []]])))
