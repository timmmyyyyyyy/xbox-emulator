use std::io::{self, Cursor, Read, Write};

#[link(name = "ntdll")]
extern "system" {
    fn RtlDecompressBuffer(format: u16, out: *mut u8, capacity: u32, input: *const u8, len: u32, written: *mut u32) -> i32;
}

pub struct Messages<R> {
    wire: R,
    block: Cursor<Vec<u8>>,
    block_capacity: usize,
    message_capacity: usize,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn size_header(reader: &mut impl Read) -> io::Result<Option<usize>> {
    let mut header = [0; 4];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    reader.read_exact(&mut header[1..])?;
    Ok(Some(u32::from_le_bytes(header) as usize))
}

impl<R: Read> Messages<R> {
    pub fn new(wire: R, block_capacity: usize, message_capacity: usize) -> Self {
        Self { wire, block: Cursor::new(Vec::new()), block_capacity, message_capacity }
    }

    fn refill(&mut self) -> io::Result<bool> {
        let Some(total) = size_header(&mut self.wire)? else { return Ok(false) };
        if !(8..=self.block_capacity).contains(&total) {
            return Err(invalid("invalid PIX transport block length"));
        }
        let mut size = [0; 4];
        self.wire.read_exact(&mut size)?;
        let decoded = u32::from_le_bytes(size);
        if decoded as usize > self.block_capacity {
            return Err(invalid("invalid PIX decoded block length"));
        }
        let mut payload = vec![0; total - 8];
        self.wire.read_exact(&mut payload)?;
        if payload.len() != decoded as usize {
            let mut output = vec![0; decoded as usize];
            let mut written = 0;
            let status = unsafe {
                RtlDecompressBuffer(3, output.as_mut_ptr(), decoded, payload.as_ptr(), payload.len() as u32, &mut written)
            };
            if status < 0 || written != decoded {
                return Err(invalid("invalid PIX XPRESS block"));
            }
            payload = output;
        }
        self.block = Cursor::new(payload);
        Ok(true)
    }

    pub fn next_message(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some(total) = size_header(self)? else { return Ok(None) };
        if !(12..=self.message_capacity).contains(&total) {
            return Err(invalid("invalid PIX message length"));
        }
        let mut message = vec![0; total];
        message[..4].copy_from_slice(&(total as u32).to_le_bytes());
        self.read_exact(&mut message[4..])?;
        Ok(Some(message))
    }
}

impl<R: Read + Write> Messages<R> {
    pub(crate) fn serve_19041(&mut self, init_flags: u32) -> io::Result<()> {
        while let Some(request) = self.next_message()? {
            self.dispatch_19041(&request, init_flags)?;
        }
        Ok(())
    }

    fn dispatch_19041(&mut self, request: &[u8], init_flags: u32) -> io::Result<()> {
        if request.len() < 28 || request[..4] != (request.len() as u32).to_le_bytes() {
            return Err(invalid("incomplete PIX device request"));
        }
        let opcode = u32::from_le_bytes([request[16], request[17], request[18], request[19]]) & 0x3fff_ffff;
        let mut storage = [0u8; 208];
        let (reply, status, reply_opcode): (&mut [u8], u32, u32) = match opcode {
            1200..=1202 => return Ok(()),
            1305 if init_flags & 1 != 0 => return Ok(()),
            1305 => (&mut storage, 0, opcode),
            1306 => (&mut storage[..84], 0, opcode),
            36 | 1204 => (&mut storage[..68], 0x8000_4005, 0),
            1205..=1206 | 1300..=1303 | 1307 | 1308 | 1310 => {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "unimplemented PIX device request"));
            }
            _ if init_flags & 1 != 0 => return Ok(()),
            _ => (&mut storage[..68], 0x8000_4001, opcode),
        };
        if request.len() < 44 {
            return Err(invalid("incomplete PIX request identifiers"));
        }
        let size = reply.len() as u32;
        reply[..4].copy_from_slice(&size.to_le_bytes());
        reply[4..8].copy_from_slice(&request[4..8]);
        reply[12..20].copy_from_slice(&request[12..20]);
        reply[20..24].copy_from_slice(&status.to_le_bytes());
        reply[24..28].copy_from_slice(&((size - 28) / 4).to_le_bytes());
        reply[28..44].copy_from_slice(&request[28..44]);
        reply[44..48].copy_from_slice(&reply_opcode.to_le_bytes());
        if matches!(opcode, 1305 | 1306) {
            reply[52..56].copy_from_slice(&0x4a61_0f6cu32.to_le_bytes());
            reply[60..64].copy_from_slice(&0x31064u32.to_le_bytes());
        }
        if opcode == 1305 {
            let labels = "August 2020 XDK QFE 4\0August 2020 QFE 4\0vb_release_xbox_dev_2008_gdk\0";
            for (dst, unit) in reply[68..].chunks_exact_mut(2).zip(labels.encode_utf16()) {
                dst.copy_from_slice(&unit.to_le_bytes());
            }
        } else if opcode == 1306 {
            for word in reply[68..].chunks_exact_mut(4) {
                word.copy_from_slice(&1u32.to_le_bytes());
            }
        }
        if self.block_capacity <= 8 || self.message_capacity < reply.len() {
            return Err(invalid("PIX transport capacity too small for device reply"));
        }
        for payload in reply.chunks(self.block_capacity - 8) {
            let size = payload.len() as u32;
            self.wire.write_all(&(size + 8).to_le_bytes())?;
            self.wire.write_all(&size.to_le_bytes())?;
            self.wire.write_all(payload)?;
        }
        self.wire.flush()
    }
}

impl<R: Read> Read for Messages<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.block.read(out)?;
            if count != 0 || !self.refill()? {
                return Ok(count);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(bytes: &[u8], decoded: u32) -> Vec<u8> {
        let mut wire = ((8 + bytes.len()) as u32).to_le_bytes().to_vec();
        wire.extend(decoded.to_le_bytes());
        wire.extend(bytes);
        wire
    }

    #[test]
    fn messages_cross_raw_and_xpress_blocks() {
        let mut message = 17u32.to_le_bytes().to_vec();
        message.extend([7; 13]);
        let mut first = message.clone();
        first.extend(&message[..2]);
        let mut input = block(&first, first.len() as u32);
        let mut literals = vec![0; 4];
        literals[..4].copy_from_slice(&((1u32 << (32 - 15)) - 1).to_le_bytes());
        literals.extend(&message[2..]);
        input.extend(block(&literals, 15));
        let mut stream = Messages::new(input.as_slice(), 64, 32);
        assert_eq!(stream.next_message().unwrap(), Some(message.clone()));
        assert_eq!(stream.next_message().unwrap(), Some(message));
        assert!(stream.next_message().unwrap().is_none());
    }

    #[test]
    fn rejected_requests_emit_no_reply() {
        let mut session = Messages::new(Cursor::new(Vec::new()), 64, 128);
        let mut query = [0u8; 44];
        query[..4].copy_from_slice(&44u32.to_le_bytes());
        query[16..20].copy_from_slice(&1066u32.to_le_bytes());
        session.dispatch_19041(&query, 1).unwrap();
        query[16..20].copy_from_slice(&1305u32.to_le_bytes());
        session.dispatch_19041(&query, 1).unwrap();
        query[16..20].copy_from_slice(&1205u32.to_le_bytes());
        assert_eq!(session.dispatch_19041(&query, 0).unwrap_err().kind(), io::ErrorKind::Unsupported);
        query[16..20].copy_from_slice(&1306u32.to_le_bytes());
        assert_eq!(session.dispatch_19041(&query[..43], 0).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(session.wire.get_ref().is_empty());
    }

    #[test]
    fn expands_an_overlapping_xpress_match() {
        let mut raw = 47u32.to_le_bytes().to_vec();
        raw.extend([0; 8]);
        let mut input = block(&raw, 12);
        let payload = [0xff, 0xff, 0xff, 0x7f, b'A', 7, 0, 15, 9];
        input.extend(block(&payload, 35));
        let mut stream = Messages::new(input.as_slice(), 64, 64);
        raw.extend([b'A'; 35]);
        assert_eq!(stream.next_message().unwrap(), Some(raw));
        assert!(stream.next_message().unwrap().is_none());
    }

    #[test]
    fn truncated_blocks_and_messages_are_errors() {
        let bytes = [12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let frame = block(&bytes, 12);
        for end in 1..frame.len() {
            let mut stream = Messages::new(&frame[..end], 64, 64);
            assert_eq!(stream.next_message().unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        }
        for (input, kind) in [
            (block(&bytes[..4], 4), io::ErrorKind::UnexpectedEof),
            (block(&bytes, 128), io::ErrorKind::InvalidData),
            (block(&[0xff; 8], 16), io::ErrorKind::InvalidData),
            (block(&[10, 0, 0, 0], 4), io::ErrorKind::InvalidData),
        ] {
            assert_eq!(Messages::new(input.as_slice(), 64, 64).next_message().unwrap_err().kind(), kind);
        }
    }
}
