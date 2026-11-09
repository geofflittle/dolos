{-# LANGUAGE FlexibleContexts #-}

-- | Builds UPLC terms and the script bytes of a program made of one.
module UplcTerm (T, int, bytes, unit, con, builtin, forced, forcedTimes, lam, var, apply, check, equal, scriptHex) where

import qualified Data.ByteString as BS
import qualified Data.ByteString.Base16 as B16
import qualified Data.ByteString.Short as SBS
import PlutusCore (DefaultFun (..), DefaultUni)
import qualified PlutusCore as PLC
import PlutusCore.DeBruijn (DeBruijn (..))
import PlutusCore.Version (plcVersion110)
import qualified PlutusLedgerApi.Common as P
import qualified UntypedPlutusCore as UPLC

-- | A term of a program.
type T = UPLC.Term DeBruijn DefaultUni DefaultFun ()

-- | An integer constant.
int :: Integer -> T
int = UPLC.Constant () . PLC.someValue

-- | A bytestring constant.
bytes :: BS.ByteString -> T
bytes = UPLC.Constant () . PLC.someValue

-- | The unit constant.
unit :: T
unit = UPLC.Constant () (PLC.someValue ())

-- | A constant of any type the default universe holds.
con :: PLC.HasTermLevel DefaultUni a => a -> T
con = UPLC.Constant () . PLC.someValue

-- | A builtin applied to its arguments.
builtin :: DefaultFun -> [T] -> T
builtin = forcedTimes 0

-- | A builtin of one type variable, forced once and applied to its arguments.
forced :: DefaultFun -> [T] -> T
forced = forcedTimes 1

-- | A builtin forced once for each of its type variables and applied to its arguments.
forcedTimes :: Int -> DefaultFun -> [T] -> T
forcedTimes n f = foldl (UPLC.Apply ()) (iterate (UPLC.Force ()) (UPLC.Builtin () f) !! n)

-- | A function of one argument.
lam :: T -> T
lam = UPLC.LamAbs () (DeBruijn 0)

-- | The innermost bound variable.
var :: T
var = UPLC.Var () (DeBruijn 1)

-- | A function term applied to an argument term.
apply :: T -> T -> T
apply = UPLC.Apply ()

-- | Unit when the boolean term is true, otherwise an error.
check :: T -> T
check b =
  UPLC.Force () $
    foldl (UPLC.Apply ()) (UPLC.Force () (UPLC.Builtin () IfThenElse)) [b, UPLC.Delay () unit, UPLC.Delay () (UPLC.Error ())]

-- | Whether two integer terms are equal.
equal :: T -> T -> T
equal a b = builtin EqualsInteger [a, b]

-- | The hex of the CBOR wrapped script bytes of a UPLC 1.1.0 program whose body is the term.
scriptHex :: T -> BS.ByteString
scriptHex body = B16.encode (SBS.fromShort (P.serialiseUPLC (UPLC.Program () plcVersion110 body)))
