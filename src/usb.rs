use rand::random;
use zerocopy::FromBytes;

const DIRECTION_OUT: u8 = 0x00;
const DIRECTION_IN: u8 = 0x80;

#[derive(FromBytes, Default)]
#[repr(C, packed)]
pub struct Cbwcb {
    pub oper_code: u8,
    pub reserved: u8,
    pub address: u32,
    pub reserved2: u8,
    pub length: u16,
    pub reserved3: [u8; 7],
}

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct Cbw<T: FromBytes> {
    pub signature: u32,
    pub tag: u32,
    pub data_transfer_length: u32,
    pub flags: u8,
    pub lun: u8,
    pub cb_length: u8,
    pub cb: T,
}

impl<T: FromBytes> Cbw<T> {
    /// Create a new CBW with defaults. Caller must set `cb` fields as required.
    pub fn new(cb: T) -> Self {
        Self {
            signature: 0x43425355, // 'USBC'
            tag: random::<u32>(),
            data_transfer_length: 0,
            flags: DIRECTION_OUT,
            lun: 0,
            cb_length: std::mem::size_of::<T>() as u8,
            cb,
        }
    }
}

impl Cbw<Cbwcb> {
    /// Initialize a CBW based on the operation code, mirroring RKComm::InitializeCBW.
    pub fn with_opcode(opcode: u8) -> Self {
        let mut cbw = Self::new(Cbwcb::default());
        cbw.cb.oper_code = opcode;

        match opcode {
            // OUT only, 6-byte command block
            0x00 /* TEST_UNIT_READY */
            | 0x01 /* READ_FLASH_ID */
            | 0x1A /* READ_FLASH_INFO */
            | 0x1B /* READ_CHIP_INFO */
            | 0x20 /* READ_EFUSE */
            | 0xAA /* READ_CAPABILITY */
            | 0x2B /* READ_STORAGE */
            | 0xFF /* DEVICE_RESET */
            | 0x16 /* ERASE_SYSTEMDISK */
            | 0x1E /* SET_RESET_FLAG */
            | 0x2A /* CHANGE_STORAGE */ => {
                cbw.flags = DIRECTION_OUT;
                cbw.cb_length = 0x06;
            }

            // IN commands (10-byte CDB)
            0x03 /* TEST_BAD_BLOCK */
            | 0x04 /* READ_SECTOR/READ_LBA */
            | 0x17 /* READ_SDRAM */
            | 0x21 /* READ_SPI_FLASH */
            | 0x24 /* READ_NEW_EFUSE */
            | 0x05 /* WRITE_SECTOR */
            | 0x15 /* WRITE_LBA */
            | 0x18 /* WRITE_SDRAM */
            | 0x19 /* EXECUTE_SDRAM */
            | 0x06 /* ERASE_NORMAL */
            | 0x0B /* ERASE_FORCE */
            | 0x1F /* WRITE_EFUSE */
            | 0x22 /* WRITE_SPI_FLASH */
            | 0x23 /* WRITE_NEW_EFUSE */
            | 0x25 /* ERASE_LBA */ => {
                cbw.flags = DIRECTION_IN;
                cbw.cb_length = 0x0A;
            }

            _ => {
                // Unknown operation code: leave defaults (caller can override)
            }
        }

        cbw
    }
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

#[derive(FromBytes)]
#[repr(C, packed)]
pub struct Csw {
    pub signature: u32,
    pub tag: u32,
    pub data_residue: u32,
    pub status: u8,
}
