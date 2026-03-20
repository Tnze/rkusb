use std::{
    fs::File,
    io::{Read, Seek},
};

use memmap2::Mmap;
use rkusb::image::{BootImage, RKBOOT_TAG, RKFW_TAG, RKLDR_TAG, RkFwImage};

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
        RKLDR_TAG | RKBOOT_TAG => ldr_info(&mut file)?,
        RKFW_TAG => rkfw_info(&mut file)?,
        x => println!("Unknown tag: {x:#04X}"),
    }
    Ok(())
}

fn ldr_info(file: &mut File) -> Result<(), Box<dyn std::error::Error>> {
    // file.lock_shared()?;
    // Safety: the file is locked so no-one can modify it.
    let mmap = unsafe { Mmap::map(&*file)? };
    let boot_img = BootImage::new(&mmap[..]);
    println!("{boot_img:#?}");

    // drop(mmap);
    // file.unlock()?;
    Ok(())
}

fn rkfw_info(file: &mut File) -> Result<(), Box<dyn std::error::Error>> {
    let mmap = unsafe { Mmap::map(&*file)? };
    let fw_img = RkFwImage::new(&mmap[..])?;
    println!("{:#?}", fw_img);
    Ok(())
}
