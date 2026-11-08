{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Writes every constructor of the Dijkstra ledger failure closure that holds no failure of a rule below it, one line each with its type.
module Main (main) where

import Cardano.Ledger.Conway.Rules (ConwayCertPredFailure, ConwayCertsPredFailure, ConwayDelegPredFailure, ConwayUtxosPredFailure)
import Cardano.Ledger.Dijkstra (DijkstraEra)
import Cardano.Ledger.Dijkstra.Rules
import Data.Kind (Type)
import Data.List (nub, sort)
import Data.Typeable (Typeable, tyConName, typeRep, typeRepTyCon)
import Data.Proxy (Proxy (..))
import GHC.Generics

-- | The leaf constructors of a failure type and of every failure type its constructors hold, each with the type that declares it.
leaves :: forall a. (Generic a, Typeable a, Constructors (Rep a)) => [(String, String)]
leaves = concatMap leaf (constructors @(Rep a))
  where
    name = tyConName (typeRepTyCon (typeRep (Proxy :: Proxy a)))
    leaf (c, Nothing) = [(c, name)]
    leaf (_, Just below) = below

-- | Each constructor of a representation with the leaves of the failure it holds, none for a leaf.
class Constructors (f :: Type -> Type) where
  constructors :: [(String, Maybe [(String, String)])]

instance Constructors f => Constructors (D1 d f) where
  constructors = constructors @f

instance (Constructors f, Constructors g) => Constructors (f :+: g) where
  constructors = constructors @f ++ constructors @g

instance Constructors V1 where
  constructors = []

instance (Constructor c, Fields f) => Constructors (C1 c f) where
  constructors = [(conName (undefined :: C1 c f ()), held)]
    where
      held = case fields @f of
        [] -> Nothing
        below -> Just (concat below)

-- | The leaves of each field of a constructor that holds a failure of a rule below.
class Fields (f :: Type -> Type) where
  fields :: [[(String, String)]]

instance Fields U1 where
  fields = []

instance (Fields f, Fields g) => Fields (f :*: g) where
  fields = fields @f ++ fields @g

instance Failure a => Fields (S1 s (K1 i a)) where
  fields = maybe [] pure (failure @a)

-- | The leaves of a field type that is a failure of the closure, none for any other type.
class Failure a where
  failure :: Maybe [(String, String)]

instance {-# OVERLAPPABLE #-} Failure a where
  failure = Nothing

instance Failure (DijkstraUtxowPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraUtxowPredFailure DijkstraEra))
instance Failure (DijkstraUtxoPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraUtxoPredFailure DijkstraEra))
instance Failure (ConwayUtxosPredFailure DijkstraEra) where failure = Just (leaves @(ConwayUtxosPredFailure DijkstraEra))
instance Failure (EntitiesPredFailure DijkstraEra) where failure = Just (leaves @(EntitiesPredFailure DijkstraEra))
instance Failure (ConwayCertsPredFailure DijkstraEra) where failure = Just (leaves @(ConwayCertsPredFailure DijkstraEra))
instance Failure (ConwayCertPredFailure DijkstraEra) where failure = Just (leaves @(ConwayCertPredFailure DijkstraEra))
instance Failure (ConwayDelegPredFailure DijkstraEra) where failure = Just (leaves @(ConwayDelegPredFailure DijkstraEra))
instance Failure (DijkstraGovCertPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraGovCertPredFailure DijkstraEra))
instance Failure (DijkstraPoolPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraPoolPredFailure DijkstraEra))
instance Failure (DijkstraGovPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraGovPredFailure DijkstraEra))
instance Failure (DijkstraSubLedgersPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubLedgersPredFailure DijkstraEra))
instance Failure (DijkstraSubLedgerPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubLedgerPredFailure DijkstraEra))
instance Failure (DijkstraSubUtxowPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubUtxowPredFailure DijkstraEra))
instance Failure (DijkstraSubUtxoPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubUtxoPredFailure DijkstraEra))
instance Failure (SubEntitiesPredFailure DijkstraEra) where failure = Just (leaves @(SubEntitiesPredFailure DijkstraEra))
instance Failure (DijkstraSubCertsPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubCertsPredFailure DijkstraEra))
instance Failure (DijkstraSubCertPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubCertPredFailure DijkstraEra))
instance Failure (DijkstraSubDelegPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubDelegPredFailure DijkstraEra))
instance Failure (DijkstraSubGovCertPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubGovCertPredFailure DijkstraEra))
instance Failure (DijkstraSubPoolPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubPoolPredFailure DijkstraEra))
instance Failure (DijkstraSubGovPredFailure DijkstraEra) where failure = Just (leaves @(DijkstraSubGovPredFailure DijkstraEra))

main :: IO ()
main =
  writeFile "/probe/out/leaves.txt" . unlines . map (\(c, t) -> c ++ " " ++ t) . nub . sort $
    leaves @(DijkstraLedgerPredFailure DijkstraEra)
