//! Wrap [`Affine`] and [`Scalar`] into secret key and private key
//!
//! The libsecp256k1 library is still available,
//! but for ease of use, further encapsulation.

use arrayref::{array_mut_ref, array_ref};

use log::warn;
use secp256k1::{
    curve::{Affine, Field, Jacobian, Scalar, ECMULT_CONTEXT, ECMULT_GEN_CONTEXT},
    util::{TAG_PUBKEY_EVEN, TAG_PUBKEY_FULL, TAG_PUBKEY_ODD},
};

use super::error::Musig2Error;
use core::convert::TryFrom;
use core::ops::Neg;
use light_bitcoin_schnorr::{taggedhash::HashInto, xonly::XOnly};
use rand_core::{OsRng, RngCore};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PublicKey(pub Affine);

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PrivateKey(pub Scalar);

/// Public key multiplication and addition calculations
impl PublicKey {
    pub fn add_point(&self, rhs: &Self) -> Result<PublicKey, Musig2Error> {
        let mut qj = Jacobian::default();
        qj.set_infinity();
        qj = qj.add_ge(&self.0);
        qj = qj.add_ge(&rhs.0);

        if qj.is_infinity() {
            return Err(Musig2Error::InvalidPublicKey);
        }
        let q = Affine::from_gej(&qj);
        Ok(PublicKey(q))
    }

    pub fn mul_scalar(&self, rhs: &PrivateKey) -> Result<PublicKey, Musig2Error> {
        if rhs.0.is_zero() {
            return Err(Musig2Error::InvalidPrivateKey);
        }
        let mut r = Jacobian::default();
        let zero = Scalar::from_int(0);
        let pt = Jacobian::from_ge(&self.0);
        ECMULT_CONTEXT.ecmult(&mut r, &pt, &rhs.0, &zero);

        Ok(PublicKey(Affine::from_gej(&r)))
    }
}

/// Secret key multiplication and addition calculations
impl PrivateKey {
    pub fn add_scalar(&self, rhs: &Self) -> Result<Self, Musig2Error> {
        let v = &self.0 + &rhs.0;
        if v.is_zero() {
            return Err(Musig2Error::InvalidPrivateKey);
        }
        Ok(PrivateKey(v))
    }

    pub fn mul_scalar(&self, rhs: &Self) -> Result<Self, Musig2Error> {
        let v = &self.0 * &rhs.0;
        if v.is_zero() {
            return Err(Musig2Error::InvalidPrivateKey);
        }
        Ok(PrivateKey(v))
    }

    pub fn mul_point(&self, rhs: &PublicKey) -> Result<PublicKey, Musig2Error> {
        if self.0.is_zero() {
            return Err(Musig2Error::InvalidPrivateKey);
        }
        let mut r = Jacobian::default();
        let zero = Scalar::from_int(0);
        let pt = Jacobian::from_ge(&rhs.0);
        ECMULT_CONTEXT.ecmult(&mut r, &pt, &self.0, &zero);

        Ok(PublicKey(Affine::from_gej(&r)))
    }
}

impl From<Affine> for PublicKey {
    fn from(p: Affine) -> Self {
        PublicKey(p)
    }
}

impl From<PublicKey> for Affine {
    fn from(p: PublicKey) -> Self {
        p.0
    }
}

impl From<Scalar> for PrivateKey {
    fn from(s: Scalar) -> Self {
        PrivateKey(s)
    }
}

impl From<PrivateKey> for Scalar {
    fn from(s: PrivateKey) -> Self {
        s.0
    }
}
impl From<PublicKey> for XOnly {
    fn from(pk: PublicKey) -> Self {
        Self(pk.x_coor())
    }
}

impl TryFrom<&str> for PrivateKey {
    type Error = Musig2Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if let Ok(x_bytes) = hex::decode(value) {
            if x_bytes.len() != 32 {
                return Err(Musig2Error::InvalidInputLength);
            }
            Self::parse_slice(&x_bytes[..])
        } else {
            Err(Musig2Error::Invalid)
        }
    }
}

impl TryFrom<&str> for PublicKey {
    type Error = Musig2Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let x_bytes = hex::decode(value)?;
        if x_bytes.len() != 32 {
            return Err(Musig2Error::InvalidStringLength);
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&x_bytes);
        PublicKey::parse_x_coor(&k)
    }
}

impl HashInto for PrivateKey {
    fn hash_into(&self, hash: &mut impl digest::Digest) {
        hash.update(self.0.b32())
    }
}

impl PublicKey {
    pub fn serialize_compressed(&self) -> [u8; 33] {
        debug_assert!(!self.0.is_infinity());

        let mut ret = [0u8; 33];
        let mut elem = self.0.clone();

        elem.x.normalize_var();
        elem.y.normalize_var();
        elem.x.fill_b32(array_mut_ref!(ret, 1, 32));
        ret[0] = if elem.y.is_odd() {
            TAG_PUBKEY_ODD
        } else {
            TAG_PUBKEY_EVEN
        };

        ret
    }

    pub fn serialize(&self) -> [u8; 65] {
        debug_assert!(!self.0.is_infinity());

        let mut ret = [0u8; 65];
        let mut elem = self.0.clone();

        elem.x.normalize_var();
        elem.y.normalize_var();
        elem.x.fill_b32(array_mut_ref!(ret, 1, 32));
        elem.y.fill_b32(array_mut_ref!(ret, 33, 32));
        ret[0] = TAG_PUBKEY_FULL;

        ret
    }

    pub fn x_coor(&self) -> [u8; 32] {
        let mut x = self.0.x.clone();
        x.normalize();
        x.b32()
    }

    pub fn y_coor(&self) -> [u8; 32] {
        let mut y = self.0.y.clone();
        y.normalize();
        y.b32()
    }

    pub fn is_odd_y(&self) -> bool {
        let mut y = self.0.y.clone();
        y.normalize();
        y.is_odd()
    }

    pub fn create_from_private_key(s: &PrivateKey) -> PublicKey {
        let mut pj = Jacobian::default();
        ECMULT_GEN_CONTEXT.ecmult_gen(&mut pj, &s.0);
        let mut p = Affine::default();
        p.set_gej(&pj);
        PublicKey(p)
    }

    pub fn neg(&self) -> PublicKey {
        // let p: Affine = self.0.clone();
        // let p = p.neg();
        PublicKey(self.0.neg())
    }

    pub fn parse(p: &[u8; 65]) -> Result<Self, Musig2Error> {
        let mut x = Field::default();
        let mut y = Field::default();
        if !x.set_b32(array_ref!(p, 1, 32)) {
            return Err(Musig2Error::InvalidPublicKey);
        }

        if !y.set_b32(array_ref!(p, 33, 32)) {
            return Err(Musig2Error::InvalidPublicKey);
        }
        let mut elem = Affine::default();
        elem.set_xy(&x, &y);

        if elem.is_infinity() {
            return Err(Musig2Error::InvalidPublicKey);
        }

        if !elem.is_valid_var() {
            return Err(Musig2Error::InvalidPublicKey);
        }
        Ok(PublicKey(elem))
    }

    /// Convert [`x_coor`] to [`PublicKey`]
    ///
    /// Recover the public key from the x coordinate in the schnorr signature;
    /// Reference ift_x(x): [BIP340]: https://github.com/bitcoin/bips/blob/master/bip-0340.mediawiki
    pub fn parse_x_coor(x: &[u8; 32]) -> Result<Self, Musig2Error> {
        let mut elem = Field::default();
        let mut affine = Affine::default();
        if elem.set_b32(x) && affine.set_xo_var(&elem, false) {
            Ok(Self(affine))
        } else {
            Err(Musig2Error::XCoordinateNotExist)
        }
    }

    pub fn convert_from_vec(v: Vec<Vec<u8>>) -> Vec<Self> {
        let mut pks = vec![];
        for i in v.iter() {
            let mut tt = [0u8; 65];

            tt.copy_from_slice(i.as_slice());

            if let Ok(pk) = PublicKey::parse(&tt) {
                pks.push(pk);
            } else {
                warn!("pks push failed, `PublicKey::parse(&tt)` meet error");
            }
        }
        pks
    }
    pub fn convert_to_vec(pks: Vec<PublicKey>) -> Vec<Vec<u8>> {
        let mut vv = vec![];
        for v in pks.iter() {
            vv.push(v.serialize().to_vec());
        }
        vv
    }
}

impl PrivateKey {
    pub fn serialize(&self) -> [u8; 32] {
        self.0.b32()
    }

    pub fn parse(s: &[u8; 32]) -> Result<Self, Musig2Error> {
        let mut r = Scalar::default();
        if !bool::from(r.set_b32(s)) {
            Ok(PrivateKey(r))
        } else {
            Err(Musig2Error::InvalidPrivateKey)
        }
    }

    pub fn parse_slice(s: &[u8]) -> Result<Self, Musig2Error> {
        if s.len() != 32 {
            return Err(Musig2Error::InvalidInputLength);
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(s);
        Self::parse(&k)
    }

    pub fn neg(&self) -> Self {
        PrivateKey(self.0.clone().neg())
    }

    pub fn generate_random() -> Result<Self, Musig2Error> {
        let mut key: [u8; 32] = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        Self::parse(&key)
    }
}
