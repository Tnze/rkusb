use std::{
    fs::File,
    io::{Read, Seek},
};

use memmap2::Mmap;
use rkusb::image::{BootImage, IDBlockHeader, RkBootEntryHeader, RkBootEntryType};

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

    match zerocopy::little_endian::U32::from_bytes(tag).get() {
        0x2052444C => ldr_info(&mut file)?,
        x => println!("Unknown tag: {x:#04X}"),
    }
    Ok(())
}

fn ldr_info(file: &mut File) -> Result<(), Box<dyn std::error::Error>> {
    // TODO: Safety: lock the file while we are reading it
    let mmap = unsafe { Mmap::map(&*file)? };
    let boot_img = BootImage::new(&mmap[..]);

    unsafe {
        let idblock = boot_img.get_idblock();
        println!("{:#X?}", std::ptr::read_unaligned(idblock));

        for i in 0..(*idblock).entry_741_count as usize {
            ldr_entry_info(boot_img.get_entry(RkBootEntryType::Entry471, i), i);
        }
        for i in 0..(*idblock).entry_742_count as usize {
            ldr_entry_info(boot_img.get_entry(RkBootEntryType::Entry472, i), i);
        }
        for i in 0..(*idblock).loader_entry_count as usize {
            ldr_entry_info(boot_img.get_entry(RkBootEntryType::EntryLoader, i), i);
        }
    }
    Ok(())
}

unsafe fn ldr_entry_info(entry: *const RkBootEntryHeader, idx: usize) {
    unsafe {
        let RkBootEntryHeader {
            size,
            r#type,
            name,
            data_offset,
            data_size,
            data_delay,
        } = *entry;

        println!(
            "{type:?}[{idx}] {{ size: {size:#X}, name: {}, data_offset: {data_offset}, data_size: {data_size}, data_delay: {data_delay} }}",
            String::from_utf16_lossy(&name[..])
        );
    }
}
