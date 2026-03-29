use std::{
    borrow::Cow,
    fs::File,
    io::{Read, Seek},
};

use memmap2::Mmap;
use rc4::{KeyInit, StreamCipher};
use rkusb::{
    idblock::{
        RC4_KEY, Rc4Cipher,
        new::{RKNS_TAG, RkNsImage},
    },
    image::{RKBOOT_TAG, RKFW_TAG, RKLDR_TAG, RkBootEntryType, RkBootImage, RkFwImage},
};

#[derive(clap::Args)]
pub struct Args {
    #[arg(help = "The image file path, update.img, rk3588_spl_loader_v1.19.113.bin, etc")]
    path: String,
}

pub fn exec(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::open(&args.path)?;

    println!("Size: {:#X}", file.metadata()?.len());

    let mut tag = [0u8; 4];
    file.seek(std::io::SeekFrom::Start(0x0))?;
    file.read_exact(&mut tag)?;

    // file.lock_shared()?;
    // Safety: the file is locked so no-one can modify it.
    let mmap = unsafe { Mmap::map(&file)? };

    match zerocopy::little_endian::U32::from_bytes(tag).get() {
        RKLDR_TAG | RKBOOT_TAG => {
            let boot_img = RkBootImage::new(&mmap[..])?;
            println!("{boot_img:#?}");
            dump_flash_head_idblock(&boot_img);
        }
        RKFW_TAG => {
            let fw_img = RkFwImage::new(&mmap[..])?;
            println!("{fw_img:#?}");
        }
        RKNS_TAG => {
            let rkns_header = RkNsImage::new(&mmap[..])?;
            println!("{rkns_header:#?}");
        }
        x => println!("Unknown tag: {x:#06X}"),
    }

    // drop(mmap);
    // file.unlock()?;
    Ok(())
}

fn dump_flash_head_idblock(boot_img: &RkBootImage<'_>) {
    let Some((_, flash_head, _)) = boot_img
        .iter_entries(RkBootEntryType::EntryLoader)
        .find(|entry| entry.0 == "FlashHead")
    else {
        return;
    };

    let mut flash_head = Cow::Borrowed(flash_head);
    if unsafe { (*boot_img.boot_header_ptr()).rc4_flag } != 0 {
        Rc4Cipher::new((&RC4_KEY).into()).apply_keystream(flash_head.to_mut());
    }

    println!("FlashHead IDBlock: {:#X?}", RkNsImage::new(&flash_head));
}
