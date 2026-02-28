use crate::{
    amount::{Amount, NegativeAllowed},
};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ShieldedData {
    /// The actions (cv, rk, sig for each).
    pub actions: Vec<super::Action>,

    /// Net value of Tachyon spends minus outputs.
    pub value_balance: Amount<NegativeAllowed>,

    /// Binding signature on transaction sighash.
    /// None when actions is empty (nothing to sign over).
    pub binding_sig: Option<super::BindingSignature>,

    /// The stamp containing tachygrams, anchor, and proof.
    /// None after stripping during aggregation.
    pub stamp: Option<super::Stamp>,
}