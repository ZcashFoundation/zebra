// XXX generate should be rewritten as strategies
#[cfg(any(test, feature = "bench"))]
pub mod generate;
#[cfg(test)]
mod preallocate;
#[cfg(test)]
mod prop;
#[cfg(test)]
mod vectors;
