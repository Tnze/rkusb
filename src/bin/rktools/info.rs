use std::{
    fs::File,
    io::{Read, Seek},
};

use memmap2::Mmap;
use rkusb::image::{
    BootImage, RKLDR_TAG, RKBOOT_TAG, RKFW_TAG, RkBootEntryHeader, RkBootEntryType, RkFwImage,
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
    dump_boot_image(&boot_img);

    // drop(mmap);
    // file.unlock()?;
    Ok(())
}

fn rkfw_info(file: &mut File) -> Result<(), Box<dyn std::error::Error>> {
    let mmap = unsafe { Mmap::map(&*file)? };
    let fw_img = RkFwImage::new(&mmap[..]);
    println!("{:#?}", fw_img);

    if let Some(boot_img) = fw_img.boot_data() {
        dump_boot_image(&boot_img);
    } else {
        println!("Embedded boot image: invalid range");
    }

    Ok(())
}

fn dump_boot_image(boot_img: &BootImage<'_>) {
    unsafe {
        let idblock = boot_img.get_idblock();
        println!("{:#X?}", std::ptr::read_unaligned(idblock));

        for i in 0..(*idblock).entry_741_count as usize {
            ldr_entry_info(boot_img.get_entry_header(RkBootEntryType::Entry471, i), i);
        }
        for i in 0..(*idblock).entry_742_count as usize {
            ldr_entry_info(boot_img.get_entry_header(RkBootEntryType::Entry472, i), i);
        }
        for i in 0..(*idblock).loader_entry_count as usize {
            ldr_entry_info(
                boot_img.get_entry_header(RkBootEntryType::EntryLoader, i),
                i,
            );
        }
    }
    let expected_crc32 = boot_img.get_crc32();
    let calculated_ccrc32 = boot_img.calculate_crc32();
    println!(
        "CRC32 IsMatch: {}, Expected: {expected_crc32:#X}, Calculated: {calculated_ccrc32:#X}",
        expected_crc32 == calculated_ccrc32
    );
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
            "{} {{ size: {size:#X}, type: {type:?}[{idx}], data_offset: {data_offset:#X}, data_size: {data_size:#X}, data_delay: {data_delay} }}",
            String::from_utf16_lossy(&name[..])
        );
    }
}
