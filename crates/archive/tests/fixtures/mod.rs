pub struct Record<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
    pub expanded_len: u32,
    pub crc: u32,
    pub method: u16,
    pub flags: u16,
    pub mode: u16,
    pub comment: &'a [u8],
}

impl<'a> Record<'a> {
    pub fn file(name: &'a [u8], data: &'a [u8]) -> Self {
        Self {
            name,
            data,
            expanded_len: data.len().try_into().unwrap(),
            crc: crc32(data),
            method: 0,
            flags: 0,
            mode: 0o100644,
            comment: b"",
        }
    }

    pub fn dir(name: &'a [u8]) -> Self {
        Self {
            mode: 0o040755,
            ..Self::file(name, b"")
        }
    }

    pub fn deflated(name: &'a [u8], expanded: &[u8], compressed: &'a [u8]) -> Self {
        Self {
            data: compressed,
            expanded_len: expanded.len().try_into().unwrap(),
            crc: crc32(expanded),
            method: 8,
            ..Self::file(name, b"")
        }
    }

    fn sizes(&self, bytes: &mut Vec<u8>) {
        u32le(bytes, self.crc);
        u32le(bytes, self.data.len().try_into().unwrap());
        u32le(bytes, self.expanded_len);
        u16le(bytes, self.name.len().try_into().unwrap());
        u16le(bytes, 0); // extra length
    }
}

pub struct ClassicZip {
    pub bytes: Vec<u8>,
    pub directory_start: usize,
}

impl ClassicZip {
    pub fn new(records: &[Record<'_>]) -> Self {
        Self::with_directory_order(records, 0..records.len())
    }

    pub fn with_directory_order(
        records: &[Record<'_>],
        order: impl IntoIterator<Item = usize>,
    ) -> Self {
        let mut bytes = Vec::new();
        let mut offsets = Vec::new();
        for record in records {
            offsets.push(u32::try_from(bytes.len()).unwrap());
            u32le(&mut bytes, 0x0403_4b50);
            u16le(&mut bytes, 20); // version needed
            u16le(&mut bytes, record.flags);
            u16le(&mut bytes, record.method);
            u32le(&mut bytes, 0x0021_0000); // DOS date 1980-01-01
            record.sizes(&mut bytes);
            bytes.extend_from_slice(record.name);
            bytes.extend_from_slice(record.data);
        }

        let directory_start = bytes.len();
        for index in order {
            let record = &records[index];
            u32le(&mut bytes, 0x0201_4b50);
            u16le(&mut bytes, 0x0314); // Unix creator, version 2.0
            u16le(&mut bytes, 20);
            u16le(&mut bytes, record.flags);
            u16le(&mut bytes, record.method);
            u32le(&mut bytes, 0x0021_0000);
            record.sizes(&mut bytes);
            u16le(&mut bytes, record.comment.len().try_into().unwrap());
            u16le(&mut bytes, 0); // disk start
            u16le(&mut bytes, 0); // internal attributes
            let dos_dir = if record.mode & 0o170000 == 0o040000 {
                0x10
            } else {
                0
            };
            u32le(&mut bytes, (u32::from(record.mode) << 16) | dos_dir);
            u32le(&mut bytes, offsets[index]);
            bytes.extend_from_slice(record.name);
            bytes.extend_from_slice(record.comment);
        }

        let directory_size = (bytes.len() - directory_start).try_into().unwrap();
        let count = records.len().try_into().unwrap();
        u32le(&mut bytes, 0x0605_4b50);
        u16le(&mut bytes, 0); // this disk
        u16le(&mut bytes, 0); // directory disk
        u16le(&mut bytes, count);
        u16le(&mut bytes, count);
        u32le(&mut bytes, directory_size);
        u32le(&mut bytes, directory_start.try_into().unwrap());
        u16le(&mut bytes, 0); // EOCD comment length
        Self {
            bytes,
            directory_start,
        }
    }

    pub fn set_eocd(&mut self, offset: usize, value: &[u8]) {
        let start = self.bytes.len() - 22 + offset;
        self.bytes[start..start + value.len()].copy_from_slice(value);
    }
}

fn u16le(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn u32le(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |crc, byte| {
        (0..8).fold(crc ^ u32::from(*byte), |bits, _| {
            (bits >> 1) ^ if bits & 1 == 1 { 0xedb8_8320 } else { 0 }
        })
    })
}
