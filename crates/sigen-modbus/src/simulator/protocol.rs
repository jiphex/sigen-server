use super::model::Model;

/// Return one Modbus PDU, preserving the caller's MBAP transaction and unit IDs.
pub(super) fn respond(model: &mut Model, unit: u8, request: &[u8]) -> Option<Vec<u8>> {
    if unit == 0 {
        return None;
    }
    let function = request[0]; // MBAP validation guarantees at least one byte.
    let result = match function {
        3 | 4 => read(model, unit, request),
        6 => {
            if request.len() != 5 {
                Err(3)
            } else {
                model
                    .write(unit, word(request, 1), &[word(request, 3)])
                    .map(|()| request.to_vec())
            }
        }
        16 => write_multiple(model, unit, request),
        _ => Err(1),
    };
    Some(result.unwrap_or_else(|exception| vec![function | 0x80, exception]))
}
fn read(model: &mut Model, unit: u8, request: &[u8]) -> Result<Vec<u8>, u8> {
    if request.len() != 5 {
        return Err(3);
    }
    let address = word(request, 1);
    let count = word(request, 3);
    if count == 0 || count > 124 {
        return Err(3);
    }
    if u32::from(address) + u32::from(count) > 65536 {
        return Err(2);
    }
    let values = model.read(unit, address, count)?;
    let mut response = vec![request[0], (count * 2) as u8];
    response.extend(values.iter().flat_map(|v| v.to_be_bytes()));
    Ok(response)
}
fn write_multiple(model: &mut Model, unit: u8, request: &[u8]) -> Result<Vec<u8>, u8> {
    if request.len() < 6 {
        return Err(3);
    }
    let address = word(request, 1);
    let count = word(request, 3);
    if count == 0
        || count > 123
        || request[5] as u16 != count * 2
        || request.len() != 6 + count as usize * 2
    {
        return Err(3);
    }
    if u32::from(address) + u32::from(count) > 65536 {
        return Err(2);
    }
    let values: Vec<u16> = request[6..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    model.write(unit, address, &values)?;
    Ok(request[..5].to_vec())
}
fn word(bytes: &[u8], index: usize) -> u16 {
    u16::from_be_bytes([bytes[index], bytes[index + 1]])
}
