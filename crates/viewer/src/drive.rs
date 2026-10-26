//! The virtual USB drive. The adapter presents a disk image to the target; the
//! helper serves its sectors. This module identifies images and keeps the
//! shared drive: a FAT32 image that files are copied into and saved out of.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::SystemTime,
};

use anyhow::{Context, Result, bail, ensure};

const SECTOR: u64 = 512;
const MIB: u64 = 1 << 20;
/// The partition starts 1 MiB in, like drives Windows and Linux format.
const PARTITION_START: u64 = MIB;
const MIN_DRIVE: u64 = 512 * MIB;
const MAX_DRIVE: u64 = 32 << 30;
/// FAT32 cannot hold a file of 4 GiB or more.
pub const MAX_FILE: u64 = u32::MAX as u64;
const COPY_CHUNK: usize = 1 << 20;
const LABEL: [u8; 11] = *b"CRASH CART ";
const RAW_EXTENSIONS: [&str; 9] = ["img", "ima", "bin", "raw", "dd", "hdd", "dsk", "vfd", "flp"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// Sector-by-sector disk image, presented as a USB flash drive.
    Raw,
    /// Fixed or dynamic VHD, presented as a USB flash drive.
    Vhd,
    /// ISO 9660 disc image, presented as a USB CD-ROM.
    Iso,
}

/// Decide how a chosen file is presented. The helper verifies it again.
pub fn identify(path: &Path) -> Result<ImageFormat> {
    let mut file = File::open(path).context("The disk image could not be opened")?;
    let size = file.metadata()?.len();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mut read_at = |offset: u64, length: usize| -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        if offset + length as u64 > size {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    };
    let vhd = size >= SECTOR
        && (read_at(size - SECTOR, 8)? == b"conectix" || read_at(0, 8)? == b"conectix");
    let iso = read_at(32769, 5)? == b"CD001";
    if read_at(0, 8)? == b"vhdxfile" {
        bail!("VHDX disk images aren't supported. Convert it to VHD first.");
    }
    match extension.as_str() {
        "iso" if iso => Ok(ImageFormat::Iso),
        "iso" => bail!("This .iso file doesn't contain an ISO 9660 disc image."),
        "vhd" if vhd => Ok(ImageFormat::Vhd),
        "vhd" => bail!("This .vhd file doesn't contain a VHD footer."),
        _ if vhd => Ok(ImageFormat::Vhd),
        _ if RAW_EXTENSIONS.contains(&extension.as_str()) => {
            ensure!(
                size >= 64 << 10,
                "The disk image is too small to be a drive."
            );
            Ok(ImageFormat::Raw)
        }
        _ if iso => Ok(ImageFormat::Iso),
        _ => bail!(
            "Choose a disk image: .img, .iso or .vhd. To copy ordinary files, use Send files."
        ),
    }
}

/// Shared between a background copy and the window that shows it.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

impl Progress {
    fn cancelled(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "Cancelled.");
        Ok(())
    }

    fn file(&self, name: &str) {
        *self
            .current
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = name.to_owned();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// Top-level files and folders, folders first.
    pub entries: Vec<Entry>,
    pub files: u64,
    pub used: u64,
    pub capacity: u64,
}

/// One planned copy: a host file, or a folder to create, at a drive path.
struct Planned {
    source: Option<PathBuf>,
    target: Vec<String>,
    size: u64,
}

pub struct SharedDrive {
    pub path: PathBuf,
    min_size: u64,
    /// Free space left for the target beyond what is copied.
    headroom: u64,
}

impl SharedDrive {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            min_size: MIN_DRIVE,
            headroom: 256 * MIB,
        }
    }

    /// `%LOCALAPPDATA%\USB Crash Cart\Shared drive.img`, beside nothing else.
    pub fn default_location() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|base| {
            PathBuf::from(base)
                .join("USB Crash Cart")
                .join("Shared drive.img")
        })
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    pub fn list(&self) -> Result<Listing> {
        let file = File::open(&self.path).context("The shared drive could not be opened")?;
        let fs = mount(ReadOnly(file))?;
        let mut listing = Listing::default();
        for entry in fs.root_dir().iter() {
            let entry = entry?;
            let name = entry.file_name();
            if name == "."
                || name == ".."
                || entry
                    .attributes()
                    .contains(fatfs::FileAttributes::VOLUME_ID)
            {
                continue;
            }
            let (files, size) = if entry.is_dir() {
                tree_size(&entry.to_dir())?
            } else {
                (1, entry.len())
            };
            listing.files += files;
            listing.entries.push(Entry {
                name,
                size,
                is_dir: entry.is_dir(),
            });
        }
        listing.entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let stats = fs.stats()?;
        let cluster = u64::from(stats.cluster_size());
        listing.capacity = u64::from(stats.total_clusters()) * cluster;
        listing.used = listing.capacity - u64::from(stats.free_clusters()) * cluster;
        Ok(listing)
    }

    /// Copy files and folders onto the drive, replacing same-named files.
    /// The drive is created, or rebuilt larger, when they would not fit.
    pub fn add(&self, sources: &[PathBuf], progress: &Progress) -> Result<u64> {
        let plan = plan(sources)?;
        let total: u64 = plan.iter().map(|item| item.size).sum();
        progress.total.store(total, Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("The shared drive folder could not be created")?;
        }
        let needed = needed_bytes(&plan);
        if self.exists() {
            let free = {
                let fs = mount(open_rw(&self.path)?)?;
                let stats = fs.stats()?;
                u64::from(stats.free_clusters()) * u64::from(stats.cluster_size())
            };
            if needed > free.saturating_sub(MIB) {
                self.grow(needed, progress)?;
            }
        } else {
            create(&self.path, self.size_for(needed)?)?;
        }
        let file = open_rw(&self.path)?;
        let fs = mount(file)?;
        let mut copied = 0;
        for item in &plan {
            progress.cancelled()?;
            let root = fs.root_dir();
            let (name, parents) = item.target.split_last().context("Empty drive path")?;
            let mut dir = root;
            for part in parents {
                dir = open_or_create_dir(&dir, part)?;
            }
            let Some(source) = &item.source else {
                open_or_create_dir(&dir, name)?;
                continue;
            };
            progress.file(name);
            if let Err(error) = copy_in(&dir, name, source, progress) {
                // Never leave half a file behind.
                let _ = dir.remove(name);
                return Err(error);
            }
            copied += 1;
        }
        fs.unmount()
            .context("The shared drive could not be finished")?;
        Ok(copied)
    }

    /// Save everything on the drive into a new folder inside `destination`.
    pub fn export(&self, destination: &Path, progress: &Progress) -> Result<(PathBuf, u64)> {
        let file = File::open(&self.path).context("The shared drive could not be opened")?;
        let fs = mount(ReadOnly(file))?;
        let (_, total) = tree_size(&fs.root_dir())?;
        progress.total.store(total, Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        let folder = unused_folder(destination, "USB drive files");
        fs::create_dir(&folder)
            .with_context(|| format!("Could not create {}", folder.display()))?;
        let count = export_dir(&fs.root_dir(), &folder, progress)?;
        Ok((folder, count))
    }

    pub fn erase(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(error).context("The shared drive could not be erased")
            }
            _ => Ok(()),
        }
    }

    fn size_for(&self, needed: u64) -> Result<u64> {
        // Room for what is copied, plus a quarter (at least 256 MiB) for the target.
        let wanted = needed + (needed / 4).max(self.headroom);
        ensure!(
            wanted <= MAX_DRIVE,
            "These files need more than the shared drive's 32 GB. Send fewer at a time."
        );
        Ok(wanted.max(self.min_size).next_multiple_of(64 * MIB))
    }

    /// Build a larger drive holding everything already on this one.
    fn grow(&self, extra: u64, progress: &Progress) -> Result<()> {
        let staging = self.path.with_extension("img.resize");
        let _ = fs::remove_file(&staging);
        let result = (|| -> Result<()> {
            let old = mount(ReadOnly(File::open(&self.path)?))?;
            let stats = old.stats()?;
            let used = u64::from(stats.total_clusters() - stats.free_clusters())
                * u64::from(stats.cluster_size());
            create(&staging, self.size_for(used + extra)?)?;
            let new = mount(open_rw(&staging)?)?;
            copy_tree(&old.root_dir(), &new.root_dir(), progress)?;
            new.unmount()?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&staging);
            return Err(error.context("The shared drive could not be enlarged"));
        }
        fs::rename(&staging, &self.path)
            .context("The enlarged shared drive could not replace the old one")
    }
}

fn plan(sources: &[PathBuf]) -> Result<Vec<Planned>> {
    let mut plan = Vec::new();
    for source in sources {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("A file name could not be read")?
            .to_owned();
        walk(source, vec![name], &mut plan)?;
    }
    ensure!(!plan.is_empty(), "Nothing to send.");
    Ok(plan)
}

fn walk(source: &Path, target: Vec<String>, plan: &mut Vec<Planned>) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("{} could not be read", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(()); // Links and junctions could loop; their targets can be sent directly.
    }
    if metadata.is_dir() {
        plan.push(Planned {
            source: None,
            target: target.clone(),
            size: 0,
        });
        let mut children: Vec<_> = fs::read_dir(source)?.collect::<io::Result<_>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let name = child.file_name().into_string().map_err(|_| {
                anyhow::anyhow!("A file name in {} is not valid Unicode", source.display())
            })?;
            let mut path = target.clone();
            path.push(name);
            walk(&child.path(), path, plan)?;
        }
        return Ok(());
    }
    ensure!(
        metadata.len() <= MAX_FILE,
        "{} is 4 GB or larger, which a FAT32 drive cannot hold.",
        target.join("\\")
    );
    plan.push(Planned {
        source: Some(source.to_owned()),
        target,
        size: metadata.len(),
    });
    Ok(())
}

fn needed_bytes(plan: &[Planned]) -> u64 {
    // Whole clusters for every file and folder, at the largest cluster size used.
    plan.iter()
        .map(|item| item.size.next_multiple_of(32 << 10) + (32 << 10))
        .sum()
}

/// Read and write only inside the partition, so the MBR stays intact. Small
/// reads (FAT entries, directory records) come from a 64 KiB read-ahead.
struct Partition<T> {
    inner: T,
    position: u64,
    length: u64,
    cache: Vec<u8>,
    cache_at: u64,
}

const READ_AHEAD: usize = 64 << 10;

impl<T> Partition<T> {
    fn new(inner: T, length: u64) -> Self {
        Self {
            inner,
            position: 0,
            length,
            cache: Vec::new(),
            cache_at: 0,
        }
    }
}

impl<T: Read + Seek> Read for Partition<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let available = self.length.saturating_sub(self.position);
        let count = buffer
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        if count == 0 {
            return Ok(0);
        }
        let cached = self
            .position
            .checked_sub(self.cache_at)
            .filter(|&at| at < self.cache.len() as u64);
        if cached.is_none() && count < READ_AHEAD {
            let fill = READ_AHEAD.min(usize::try_from(available).unwrap_or(usize::MAX));
            self.cache.resize(fill, 0);
            self.inner
                .seek(SeekFrom::Start(PARTITION_START + self.position))?;
            let mut filled = 0;
            while filled < fill {
                match self.inner.read(&mut self.cache[filled..])? {
                    0 => break,
                    read => filled += read,
                }
            }
            self.cache.truncate(filled);
            self.cache_at = self.position;
        }
        let read = match self
            .position
            .checked_sub(self.cache_at)
            .filter(|&at| at < self.cache.len() as u64)
        {
            Some(at) => {
                let at = at as usize;
                let read = count.min(self.cache.len() - at);
                buffer[..read].copy_from_slice(&self.cache[at..at + read]);
                read
            }
            None => {
                self.inner
                    .seek(SeekFrom::Start(PARTITION_START + self.position))?;
                self.inner.read(&mut buffer[..count])?
            }
        };
        self.position += read as u64;
        Ok(read)
    }
}

impl<T: Write + Seek> Write for Partition<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let available = self.length.saturating_sub(self.position);
        let count = buffer
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        if count == 0 && !buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Past the end of the drive",
            ));
        }
        self.inner
            .seek(SeekFrom::Start(PARTITION_START + self.position))?;
        let written = self.inner.write(&buffer[..count])?;
        // Keep the read-ahead identical to the disk.
        let (start, end) = (self.position, self.position + written as u64);
        let (cache_start, cache_end) = (self.cache_at, self.cache_at + self.cache.len() as u64);
        if start < cache_end && end > cache_start {
            let from = start.max(cache_start);
            let to = end.min(cache_end);
            self.cache[(from - cache_start) as usize..(to - cache_start) as usize]
                .copy_from_slice(&buffer[(from - start) as usize..(to - start) as usize]);
        }
        self.position = end;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T> Seek for Partition<T> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.length.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position =
            next.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Seek before start"))?;
        Ok(self.position)
    }
}

/// Listing and saving never write; a stray write is an error, not a change.
struct ReadOnly(File);

impl Read for ReadOnly {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for ReadOnly {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Read-only drive access",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for ReadOnly {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.0.seek(from)
    }
}

type Fs<T> = fatfs::FileSystem<Partition<T>>;
type Dir<'a, T> = fatfs::Dir<'a, Partition<T>>;

fn open_rw(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .context("The shared drive is in use. Unplug it first.")
}

fn mount<T: Read + Write + Seek>(mut inner: T) -> Result<Fs<T>> {
    let mut mbr = [0; 512];
    inner.seek(SeekFrom::Start(0))?;
    inner
        .read_exact(&mut mbr)
        .context("The shared drive is damaged")?;
    ensure!(
        mbr[510..] == [0x55, 0xaa] && mbr[450] == 0x0c,
        "The shared drive is damaged: no FAT32 partition"
    );
    let start = u64::from(u32::from_le_bytes(mbr[454..458].try_into().unwrap())) * SECTOR;
    let length = u64::from(u32::from_le_bytes(mbr[458..462].try_into().unwrap())) * SECTOR;
    ensure!(
        start == PARTITION_START,
        "The shared drive has an unexpected layout"
    );
    let disk = Partition::new(inner, length);
    fatfs::FileSystem::new(disk, fatfs::FsOptions::new())
        .context("The shared drive's file system is damaged")
}

/// A new image: one MBR partition from 1 MiB, formatted FAT32.
fn create(path: &Path, size: u64) -> Result<()> {
    let sectors = (size - PARTITION_START) / SECTOR;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .context("The shared drive could not be created")?;
    let result = (|| -> Result<()> {
        file.set_len(size)?;
        let mut mbr = [0_u8; 512];
        let signature = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0x4352_4153, |time| {
                time.subsec_nanos() ^ time.as_secs() as u32
            });
        mbr[440..444].copy_from_slice(&signature.to_le_bytes());
        let entry = &mut mbr[446..462];
        entry[1..4].copy_from_slice(&chs(PARTITION_START / SECTOR));
        entry[4] = 0x0c; // FAT32 with LBA addressing
        entry[5..8].copy_from_slice(&chs(PARTITION_START / SECTOR + sectors - 1));
        entry[8..12].copy_from_slice(&((PARTITION_START / SECTOR) as u32).to_le_bytes());
        entry[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xaa;
        file.write_all(&mbr)?;
        let length = sectors * SECTOR;
        fatfs::format_volume(
            Partition::new(&mut file, length),
            fatfs::FormatVolumeOptions::new()
                .fat_type(fatfs::FatType::Fat32)
                .volume_label(LABEL),
        )?;
        // Hidden sectors record where the partition starts, in both boot sectors.
        for boot in [0, 6] {
            file.seek(SeekFrom::Start(PARTITION_START + boot * SECTOR + 0x1c))?;
            file.write_all(&((PARTITION_START / SECTOR) as u32).to_le_bytes())?;
        }
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

/// Cylinder/head/sector for an LBA with 255 heads and 63 sectors per track.
fn chs(lba: u64) -> [u8; 3] {
    let cylinder = lba / (255 * 63);
    if cylinder > 1023 {
        return [0xfe, 0xff, 0xff];
    }
    let head = (lba / 63) % 255;
    let sector = lba % 63 + 1;
    [
        head as u8,
        (sector as u8) | ((cylinder >> 2) as u8 & 0xc0),
        cylinder as u8,
    ]
}

fn open_or_create_dir<'a, T: Read + Write + Seek>(
    dir: &Dir<'a, T>,
    name: &str,
) -> Result<Dir<'a, T>> {
    match dir.open_dir(name) {
        Ok(found) => Ok(found),
        Err(_) => dir
            .create_dir(name)
            .with_context(|| format!("The folder {name} could not be created on the drive")),
    }
}

fn copy_in<T: Read + Write + Seek>(
    dir: &Dir<'_, T>,
    name: &str,
    source: &Path,
    progress: &Progress,
) -> Result<()> {
    let mut input =
        File::open(source).with_context(|| format!("{} could not be read", source.display()))?;
    let modified = input
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok();
    let mut output = dir
        .create_file(name)
        .with_context(|| format!("{name} could not be created on the drive"))?;
    output.truncate()?;
    let mut buffer = vec![0; COPY_CHUNK];
    loop {
        progress.cancelled()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .with_context(|| format!("{name} could not be written to the drive"))?;
        progress.done.fetch_add(count as u64, Ordering::Relaxed);
    }
    if let Some(time) = modified.and_then(fat_time) {
        // fatfs 0.3 prefers a TimeProvider; after the last write this keeps the source time.
        #[allow(deprecated)]
        output.set_modified(time);
    }
    output.flush()?;
    Ok(())
}

/// FAT keeps local time from 1980 to 2107 in two-second steps.
fn fat_time(time: SystemTime) -> Option<fatfs::DateTime> {
    use chrono::{Datelike, Timelike};
    let local = chrono::DateTime::<chrono::Local>::from(time);
    (1980..=2107)
        .contains(&local.year())
        .then(|| fatfs::DateTime {
            date: fatfs::Date {
                year: local.year() as u16,
                month: local.month() as u16,
                day: local.day() as u16,
            },
            time: fatfs::Time {
                hour: local.hour() as u16,
                min: local.minute() as u16,
                sec: local.second().min(59) as u16,
                millis: (local.timestamp_subsec_millis() % 1000) as u16,
            },
        })
}

/// A target can write any bits into a timestamp; invalid ones are skipped.
fn host_time(time: fatfs::DateTime) -> Option<SystemTime> {
    use chrono::TimeZone;
    let date = chrono::NaiveDate::from_ymd_opt(
        i32::from(time.date.year),
        u32::from(time.date.month),
        u32::from(time.date.day),
    )?;
    let moment = date.and_hms_milli_opt(
        u32::from(time.time.hour),
        u32::from(time.time.min),
        u32::from(time.time.sec),
        u32::from(time.time.millis),
    )?;
    Some(
        chrono::Local
            .from_local_datetime(&moment)
            .earliest()?
            .into(),
    )
}

fn tree_size<T: Read + Write + Seek>(dir: &Dir<'_, T>) -> Result<(u64, u64)> {
    let (mut files, mut bytes) = (0, 0);
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "."
            || name == ".."
            || entry
                .attributes()
                .contains(fatfs::FileAttributes::VOLUME_ID)
        {
            continue;
        }
        if entry.is_dir() {
            let (inner_files, inner_bytes) = tree_size(&entry.to_dir())?;
            files += inner_files;
            bytes += inner_bytes;
        } else {
            files += 1;
            bytes += entry.len();
        }
    }
    Ok((files, bytes))
}

fn copy_tree<A, B>(from: &Dir<'_, A>, to: &Dir<'_, B>, progress: &Progress) -> Result<()>
where
    A: Read + Write + Seek,
    B: Read + Write + Seek,
{
    for entry in from.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "."
            || name == ".."
            || entry
                .attributes()
                .contains(fatfs::FileAttributes::VOLUME_ID)
        {
            continue;
        }
        progress.cancelled()?;
        if entry.is_dir() {
            copy_tree(&entry.to_dir(), &to.create_dir(&name)?, progress)?;
        } else {
            let mut output = to.create_file(&name)?;
            io::copy(&mut entry.to_file(), &mut output)?;
            #[allow(deprecated)]
            output.set_modified(entry.modified());
            output.flush()?;
        }
    }
    Ok(())
}

fn export_dir<T: Read + Write + Seek>(
    dir: &Dir<'_, T>,
    folder: &Path,
    progress: &Progress,
) -> Result<u64> {
    let mut count = 0;
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if name == "."
            || name == ".."
            || entry
                .attributes()
                .contains(fatfs::FileAttributes::VOLUME_ID)
        {
            continue;
        }
        progress.cancelled()?;
        let path = folder.join(host_name(&name));
        if entry.is_dir() {
            fs::create_dir(&path)
                .with_context(|| format!("Could not create {}", path.display()))?;
            count += export_dir(&entry.to_dir(), &path, progress)?;
            continue;
        }
        progress.file(&name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("Could not save {}", path.display()))?;
        let mut input = entry.to_file();
        let mut buffer = vec![0; COPY_CHUNK];
        loop {
            progress.cancelled()?;
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
            progress.done.fetch_add(read as u64, Ordering::Relaxed);
        }
        if let Some(time) = host_time(entry.modified()) {
            let _ = output.set_modified(time);
        }
        count += 1;
    }
    Ok(count)
}

/// A name Windows accepts. FAT allows a few that Windows reserves.
fn host_name(name: &str) -> String {
    let mut clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = clean
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        clean.insert(0, '_');
    }
    let trimmed = clean.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        "_".into()
    } else {
        trimmed.to_owned()
    }
}

fn unused_folder(destination: &Path, name: &str) -> PathBuf {
    let mut candidate = destination.join(name);
    let mut number = 2;
    while candidate.exists() {
        candidate = destination.join(format!("{name} ({number})"));
        number += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cart-drive-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn small(path: PathBuf) -> SharedDrive {
        SharedDrive {
            path,
            min_size: 40 * MIB,
            headroom: 8 * MIB,
        }
    }

    #[test]
    fn identifies_images_by_content_and_refuses_ordinary_files() {
        let scratch = Scratch::new("identify");
        let write = |name: &str, bytes: &[u8]| {
            let path = scratch.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let mut iso = vec![0; 65536];
        iso[32769..32774].copy_from_slice(b"CD001");
        let mut vhd = vec![0; 2048];
        vhd[1536..1544].copy_from_slice(b"conectix");
        assert_eq!(
            identify(&write("disc.iso", &iso)).unwrap(),
            ImageFormat::Iso
        );
        assert_eq!(
            identify(&write("disc.dat", &iso)).unwrap(),
            ImageFormat::Iso
        );
        // A hybrid image named as a disk stays a disk.
        assert_eq!(
            identify(&write("hybrid.img", &iso)).unwrap(),
            ImageFormat::Raw
        );
        assert_eq!(
            identify(&write("disk.vhd", &vhd)).unwrap(),
            ImageFormat::Vhd
        );
        assert_eq!(
            identify(&write("disk.bin", &vec![0; 65536])).unwrap(),
            ImageFormat::Raw
        );
        assert!(identify(&write("notes.txt", b"hello")).is_err());
        assert!(identify(&write("fake.iso", &vec![0; 65536])).is_err());
        assert!(identify(&write("fake.vhd", &vec![0; 65536])).is_err());
        assert!(identify(&write("tiny.img", &[0; 512])).is_err());
        let mut vhdx = vec![0; 65536];
        vhdx[..8].copy_from_slice(b"vhdxfile");
        assert!(
            identify(&write("new.vhdx", &vhdx))
                .unwrap_err()
                .to_string()
                .contains("VHDX")
        );
    }

    #[test]
    fn creates_a_partitioned_fat32_drive_and_round_trips_files() {
        let scratch = Scratch::new("roundtrip");
        let source = scratch.0.join("source");
        fs::create_dir_all(source.join("Logs").join("Nested")).unwrap();
        fs::write(
            source.join("Logs").join("Nested").join("boot.log"),
            b"kernel line\n",
        )
        .unwrap();
        fs::write(source.join("Logs").join("empty.txt"), b"").unwrap();
        let big: Vec<u8> = (0..3_000_000_u32).map(|i| (i * 31 % 251) as u8).collect();
        fs::write(source.join("Résumé – final.pdf"), &big).unwrap();
        let drive = small(scratch.0.join("drive.img"));
        let progress = Progress::default();
        let copied = drive
            .add(
                &[source.join("Logs"), source.join("Résumé – final.pdf")],
                &progress,
            )
            .unwrap();
        assert_eq!(copied, 3);
        assert_eq!(
            progress.done.load(Ordering::Relaxed),
            progress.total.load(Ordering::Relaxed)
        );

        let image = fs::read(&drive.path).unwrap();
        assert_eq!(&image[510..512], &[0x55, 0xaa]);
        assert_eq!(image[450], 0x0c);
        assert_eq!(
            u32::from_le_bytes(image[454..458].try_into().unwrap()),
            2048
        );
        let boot = PARTITION_START as usize;
        assert_eq!(&image[boot + 82..boot + 90], b"FAT32   ");
        assert_eq!(
            u32::from_le_bytes(image[boot + 0x1c..boot + 0x20].try_into().unwrap()),
            2048
        );
        assert_eq!(&image[boot + 71..boot + 82], &LABEL);

        let listing = drive.list().unwrap();
        assert_eq!(
            listing.entries,
            vec![
                Entry {
                    name: "Logs".into(),
                    size: 12,
                    is_dir: true
                },
                Entry {
                    name: "Résumé – final.pdf".into(),
                    size: big.len() as u64,
                    is_dir: false
                },
            ]
        );
        assert_eq!(listing.files, 3);
        assert!(listing.capacity >= 39 * MIB && listing.used >= big.len() as u64);

        let saved = scratch.0.join("saved");
        fs::create_dir(&saved).unwrap();
        let (folder, count) = drive.export(&saved, &Progress::default()).unwrap();
        assert_eq!(
            (folder.file_name().unwrap().to_str().unwrap(), count),
            ("USB drive files", 3)
        );
        assert_eq!(fs::read(folder.join("Résumé – final.pdf")).unwrap(), big);
        assert_eq!(
            fs::read(folder.join("Logs").join("Nested").join("boot.log")).unwrap(),
            b"kernel line\n"
        );
        assert_eq!(
            fs::read(folder.join("Logs").join("empty.txt")).unwrap(),
            b""
        );
        // A second save never merges into or overwrites the first.
        let (again, _) = drive.export(&saved, &Progress::default()).unwrap();
        assert_eq!(again.file_name().unwrap(), "USB drive files (2)");
    }

    #[test]
    fn sending_again_replaces_same_names_and_grows_when_full() {
        let scratch = Scratch::new("grow");
        let first = scratch.0.join("first.bin");
        let second = scratch.0.join("second.bin");
        fs::write(&first, vec![1; 20 << 20]).unwrap();
        fs::write(&second, vec![2; 50 << 20]).unwrap();
        let drive = small(scratch.0.join("drive.img"));
        drive
            .add(std::slice::from_ref(&first), &Progress::default())
            .unwrap();
        let before = drive.list().unwrap().capacity;
        fs::write(&first, b"replaced").unwrap();
        drive
            .add(&[first.clone(), second.clone()], &Progress::default())
            .unwrap();
        let after = drive.list().unwrap();
        assert!(after.capacity > before, "{} > {before}", after.capacity);
        assert_eq!(
            after
                .entries
                .iter()
                .map(|e| (e.name.as_str(), e.size))
                .collect::<Vec<_>>(),
            vec![("first.bin", 8), ("second.bin", 50 << 20)]
        );
        assert!(!drive.path.with_extension("img.resize").exists());
    }

    #[test]
    fn cancelled_and_oversized_sends_leave_no_partial_files() {
        let scratch = Scratch::new("cancel");
        let file = scratch.0.join("data.bin");
        fs::write(&file, vec![7; 4 << 20]).unwrap();
        let drive = small(scratch.0.join("drive.img"));
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(drive.add(std::slice::from_ref(&file), &progress).is_err());
        assert!(drive.list().unwrap().entries.is_empty());
        assert!(plan(&[scratch.0.join("missing.bin")]).is_err());
    }

    #[test]
    fn saved_names_are_valid_on_windows() {
        assert_eq!(host_name("report.txt"), "report.txt");
        assert_eq!(host_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(host_name("CON.txt"), "_CON.txt");
        assert_eq!(host_name("com1"), "_com1");
        assert_eq!(host_name("trailing. "), "trailing");
        assert_eq!(host_name("..."), "_");
    }

    #[test]
    fn drive_sizes_leave_room_and_respect_fat32_limits() {
        let drive = SharedDrive::new(PathBuf::from("unused.img"));
        assert_eq!(drive.size_for(0).unwrap(), MIN_DRIVE);
        let size = drive.size_for(2 << 30).unwrap();
        assert!(size >= (2 << 30) + (512 * MIB) && size.is_multiple_of(64 * MIB));
        assert!(drive.size_for(30 << 30).is_err());
        assert_eq!(chs(2048), [32, 33, 0]);
        assert_eq!(chs(u64::from(u32::MAX)), [0xfe, 0xff, 0xff]);
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    #[test]
    #[ignore = "writes a hardware-check drive to CART_DRIVE_FIXTURE"]
    fn hardware_fixture() {
        let Some(path) = std::env::var_os("CART_DRIVE_FIXTURE").map(PathBuf::from) else {
            return;
        };
        let readme = path.with_file_name("CRASH-CART-CHECK.txt");
        fs::write(&readme, "USB Crash Cart virtual drive check.\r\n").unwrap();
        SharedDrive::new(path)
            .add(&[readme], &Progress::default())
            .unwrap();
    }

    #[test]
    #[ignore = "measures copy throughput on a 1 GB drive"]
    fn copy_speed() {
        let root = std::env::temp_dir().join(format!("cart-drive-speed-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let source = root.join("big.bin");
        fs::write(&source, vec![9_u8; 400 << 20]).unwrap();
        let drive = SharedDrive::new(root.join("drive.img"));
        let started = std::time::Instant::now();
        drive
            .add(std::slice::from_ref(&source), &Progress::default())
            .unwrap();
        let added = started.elapsed();
        let started = std::time::Instant::now();
        let listing = drive.list().unwrap();
        let listed = started.elapsed();
        let out = root.join("out");
        fs::create_dir(&out).unwrap();
        let started = std::time::Instant::now();
        drive.export(&out, &Progress::default()).unwrap();
        println!(
            "SPEED add={added:?} list={listed:?} export={:?} capacity={}",
            started.elapsed(),
            listing.capacity
        );
        let _ = fs::remove_dir_all(&root);
    }
}
