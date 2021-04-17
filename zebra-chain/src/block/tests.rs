// XXX generate should be rewritten as strategies
#[cfg(feature = "bench")]
pub mod generate;
#[cfg(test)]
mod generate;
#[cfg(test)]
mod preallocate;
#[cfg(test)]
mod prop;
#[cfg(test)]
mod vectors;
