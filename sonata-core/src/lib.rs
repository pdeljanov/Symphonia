pub mod errors;
pub mod sample;

pub mod codecs;
pub mod formats;
pub mod audio;
pub mod tags;
pub mod io;
pub mod checksum;
pub mod conv;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
