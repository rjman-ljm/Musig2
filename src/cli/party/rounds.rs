use super::{
    broadcast::{BroadcastMsgs, BroadcastMsgsStore},
    traits::{push::Push, state_machine::Msg},
    Store,
};
use crate::cli::protocals::{
    signature::{sign, sign_double_prime, verify, KeyAgg, KeyPair, FE, GE},
    State, StatePrime,
};
use curv::{elliptic::curves::traits::ECPoint, BigInt};
use serde::{Deserialize, Serialize};

/// Prepare round performs preprocessing operations to construct messages for the `Round1` of communication.
///
/// The main work of the preparation process is to generate nonce and construct messages.
#[derive(Debug)]
pub struct Prepare {
    pub my_ind: u16,
    pub key_pair: KeyPair,
    pub message: Vec<u8>,
}

impl Prepare {
    pub fn proceed<O>(self, mut output: O) -> Result<Round1>
    where
        O: Push<Msg<MessageRound1>>,
    {
        // Generate `nonce` from the held private key
        let (nonce, state1) = sign(self.key_pair.clone());

        // The message of the `Round1` needs to pass `nonce` and `public key`
        //
        // Nonce is necessary for the musig2 scheme, but due to the adoption of libp2p
        // it is difficult for participants to exchange the public key offline in advance
        // so the public key is also exchanged in the `Round1` of messages.
        output.push(Msg {
            sender: self.my_ind,
            receiver: None,
            body: MessageRound1 {
                ephemeral_keys: nonce,
                message: self.message.clone(),
                pubkey: self.key_pair.public_key,
            },
        });

        Ok(Round1 {
            my_ind: self.my_ind,
            state1,
            key_pair: self.key_pair.clone(),
            message: self.message,
        })
    }
    pub fn is_expensive(&self) -> bool {
        // We assume that computing hash is expensive operation (in real-world, it's not)
        false
    }
}

#[derive(Debug)]
pub struct Round1 {
    pub my_ind: u16,
    pub state1: State,
    pub key_pair: KeyPair,
    pub message: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageRound1 {
    pub ephemeral_keys: Vec<GE>,
    pub message: Vec<u8>,
    pub pubkey: GE,
}

impl Round1 {
    pub fn proceed<O>(self, input: BroadcastMsgs<MessageRound1>, mut output: O) -> Result<Round2>
    where
        O: Push<Msg<MessageRound2>>,
    {
        let mut pks = vec![];
        let mut received_nonce = vec![];
        let cur_ind: usize = self.my_ind.into();

        for i in 0..input.msgs.len() {
            if i + 1 == cur_ind {
                pks.push(self.key_pair.public_key);
            }
            pks.push(input.msgs[i].pubkey);
            received_nonce.push(input.msgs[i].ephemeral_keys.clone());
        }
        if input.msgs.len() + 1 == cur_ind {
            pks.push(self.key_pair.public_key);
        }
        let party_index: usize = (self.my_ind - 1) as usize;
        println!("pks:{:?}", pks);
        let key_agg = KeyAgg::key_aggregation_n(&pks, party_index);
        let (state2, sign_fragment) =
            self.state1
                .sign_prime(&self.message, &pks, received_nonce.clone(), party_index);
        let (commit, r, _) =
            self.state1
                .compute_global_params(&self.message, &pks, received_nonce, party_index);
        output.push(Msg {
            sender: self.my_ind,
            receiver: None,
            body: MessageRound2 { sign_fragment },
        });

        Ok(Round2 {
            my_ind: self.my_ind,
            commit,
            r,
            state2,
            key_pair: self.key_pair,
            key_agg,
        })
    }
    pub fn expects_messages(party_i: u16, party_n: u16) -> Store<BroadcastMsgs<MessageRound1>> {
        BroadcastMsgsStore::new(party_i, party_n)
    }

    pub fn is_expensive(&self) -> bool {
        // Sending cached message is the cheapest operation
        false
    }
}

#[derive(Debug)]
pub struct Round2 {
    pub my_ind: u16,
    pub commit: BigInt,
    pub r: GE,
    pub state2: StatePrime,
    pub key_pair: KeyPair,
    pub key_agg: KeyAgg,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageRound2 {
    pub sign_fragment: FE,
}

impl Round2 {
    pub fn proceed(self, input: BroadcastMsgs<MessageRound2>) -> Result<SignResult> {
        let mut received_round2 = vec![];
        for i in 0..input.msgs.len() {
            received_round2.push(input.msgs[i].sign_fragment);
        }
        let s = sign_double_prime(self.state2, &received_round2);

        assert!(verify(
            &s,
            &self.r.x_coor().unwrap(),
            &self.key_agg.X_tilde,
            &self.commit
        )
        .is_ok());
        println!("party index:{} verify success.", self.my_ind);
        Ok(SignResult {
            r: self.r,
            s,
            commit: self.commit,
        })
    }
    pub fn expects_messages(party_i: u16, party_n: u16) -> Store<BroadcastMsgs<MessageRound2>> {
        BroadcastMsgsStore::new(party_i, party_n)
    }
    pub fn is_expensive(&self) -> bool {
        // Round involves computing a hash, we assume it's expensive (again, in real-world it's not)
        false
    }
}

#[derive(Debug)]
pub struct SignResult {
    pub r: GE,
    pub s: FE,
    pub commit: BigInt,
}

// Messages

#[derive(Clone, Debug)]
pub struct CommittedSeed([u8; 32]);

#[derive(Clone, Debug)]
pub struct RevealedSeed {
    seed: u32,
    blinding: [u8; 32],
}

// Errors

type Result<T> = std::result::Result<T, ProceedError>;

#[derive(Debug, PartialEq)]
pub enum ProceedError {
    PartiesDidntRevealItsSeed { party_ind: Vec<u16> },
}
