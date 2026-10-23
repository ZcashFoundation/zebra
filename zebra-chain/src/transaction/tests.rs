#![allow(clippy::unwrap_in_result)]

mod preallocate;
mod prop;
mod vectors;

/// The allocating balance computation, kept independent of the borrowed implementation.
fn allocating_transparent_value_balance(
    tx: &super::Transaction,
    outputs: &std::collections::HashMap<crate::transparent::OutPoint, crate::transparent::Output>,
) -> Result<
    crate::value_balance::ValueBalance<crate::amount::NegativeAllowed>,
    crate::value_balance::ValueBalanceError,
> {
    use crate::{
        amount::{Amount, Error as AmountError, NonNegative},
        value_balance::{ValueBalance, ValueBalanceError},
    };

    let input_value = tx
        .inputs()
        .iter()
        .map(|input| input.value_from_outputs(outputs))
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(ValueBalanceError::Transparent)?
        .constrain()
        .expect("conversion from NonNegative to NegativeAllowed is always valid");
    let output_value = tx
        .outputs()
        .iter()
        .map(|output| output.value())
        .sum::<Result<Amount<NonNegative>, AmountError>>()
        .map_err(ValueBalanceError::Transparent)?
        .constrain()
        .expect("conversion from NonNegative to NegativeAllowed is always valid");

    (input_value - output_value)
        .map(ValueBalance::from_transparent_amount)
        .map_err(ValueBalanceError::Transparent)
}
