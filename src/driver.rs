use std::io;
use std::path::Path;

pub trait AssetDriver: Send + Sync {
    /// Returns the 4-byte magic for this format.
    fn magic(&self) -> &[u8; 4];

    /// Returns the canonical extension (e.g., ".cxan").
    fn extension(&self) -> &str;

    /// Returns the name of the entry file that identifies a folder of this type.
    fn entry_file(&self) -> &str;

    /// Packs the contents of a folder into a binary blob.
    fn pack(&self, folder: &Path) -> io::Result<Vec<u8>>;

    /// Unpacks a binary blob into a folder.
    fn unpack(&self, data: &[u8], folder: &Path) -> io::Result<()>;
}

pub fn get_drivers() -> Vec<Box<dyn AssetDriver>> {
    vec![
        Box::new(crate::cxan::CxanDriver),
    ]
}
