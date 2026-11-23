use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{Result, ResultExt};

pub fn decode_extension_ix_type<T: TryFrom<u8>>(ix_data: &[u8]) -> Result<T>
where T::Error: std::error::Error + Send + Sync + 'static {
    let first_byte: u8 = *ix_data.first().ok_or(shipstern_parser::Error::new(
        "Instruction data for token extension is empty",
    ))?;

    T::try_from(first_byte).parse_err("Error decoding instruction data for token extension")
}

/// Decodes the fixed-size data that follows the token and extension instruction tags.
pub fn decode_extension_ix_data<T: bytemuck::Pod>(ix_data: &[u8]) -> Result<T> {
    let data = ix_data.get(2..).ok_or(shipstern_parser::Error::new(
        "Instruction data for token extension is too short",
    ))?;

    bytemuck::try_pod_read_unaligned(data).map_err(|e| {
        shipstern_parser::Error::new(format!(
            "Error decoding instruction data for token extension: {e:?}"
        ))
    })
}

pub trait ExtensionInstructionParser: Sized {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self>;
}
