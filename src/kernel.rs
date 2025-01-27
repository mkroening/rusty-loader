//! Parsing and loading kernel objects from ELF files.
#![deny(unsafe_code)]

use crate::arch::{self, BootInfo};

use core::mem::{self, MaybeUninit};

use goblin::elf64::{
	dynamic::{self, Dyn, DynamicInfo},
	header::{self, Header},
	program_header::{self, ProgramHeader},
	reloc::{self, Rela},
};
use plain::Plain;

/// A parsed kernel object ready for loading.
pub struct Object<'a> {
	/// The raw bytes of the parsed ELF file.
	elf: &'a [u8],

	/// The ELF file header at the beginning of [`Self::elf`].
	header: &'a Header,

	/// The kernel's program headers.
	///
	/// Loadable program segments will be copied for execution.
	///
	/// The thread-local storage segment will be used for creating [`TlsInfo`] for the kernel.
	phs: &'a [ProgramHeader],

	/// Relocations with an explicit addend.
	relas: &'a [Rela],
}

impl<'a> Object<'a> {
	/// Parses raw bytes of an ELF file into a loadable kernel object.
	pub fn parse(elf: &[u8]) -> Object<'_> {
		{
			let range = elf.as_ptr_range();
			let len = elf.len();
			loaderlog!("Parsing kernel from ELF at {range:?} ({len} B)");
		}

		let header = plain::from_bytes::<Header>(elf).unwrap();

		// General compatibility checks
		{
			let class = header.e_ident[header::EI_CLASS];
			assert_eq!(header::ELFCLASS64, class, "kernel ist not a 64-bit object");
			let data_encoding = header.e_ident[header::EI_DATA];
			assert_eq!(
				header::ELFDATA2LSB,
				data_encoding,
				"kernel object is not little endian"
			);

			assert!(
				matches!(header.e_type, header::ET_DYN | header::ET_EXEC),
				"kernel has unsupported ELF type"
			);

			assert_eq!(
				arch::ELF_ARCH,
				header.e_machine,
				"kernel is not compiled for the correct architecture"
			);
		}

		let phs = {
			let start = header.e_phoff as usize;
			let len = header.e_phnum as usize;
			ProgramHeader::slice_from_bytes_len(&elf[start..], len).unwrap()
		};

		let dyns = phs
			.iter()
			.find(|program_header| program_header.p_type == program_header::PT_DYNAMIC)
			.map(|ph| {
				let start = ph.p_offset as usize;
				let len = (ph.p_filesz as usize) / dynamic::SIZEOF_DYN;
				Dyn::slice_from_bytes_len(&elf[start..], len).unwrap()
			})
			.unwrap_or_default();

		assert!(
			!dyns.iter().any(|d| d.d_tag == dynamic::DT_NEEDED),
			"kernel was linked against dynamic libraries"
		);

		let dynamic_info = DynamicInfo::new(dyns, phs);
		assert_eq!(0, dynamic_info.relcount);

		let relas = {
			let start = dynamic_info.rela;
			let len = dynamic_info.relacount;
			Rela::slice_from_bytes_len(&elf[start..], len).unwrap()
		};

		assert!(relas
			.iter()
			.all(|rela| reloc::r_type(rela.r_info) == arch::R_RELATIVE));

		Object {
			elf,
			header,
			phs,
			relas,
		}
	}

	/// Required memory size for loading.
	///
	/// Returns the minimum size of a block of memory for successfully loading the object.
	pub fn mem_size(&self) -> usize {
		let first_ph = self
			.phs
			.iter()
			.find(|ph| ph.p_type == program_header::PT_LOAD)
			.unwrap();
		let start_addr = first_ph.p_vaddr;

		let last_ph = self
			.phs
			.iter()
			.rev()
			.find(|ph| ph.p_type == program_header::PT_LOAD)
			.unwrap();
		let end_addr = last_ph.p_vaddr + last_ph.p_memsz;

		let mem_size = end_addr - start_addr;
		mem_size.try_into().unwrap()
	}

	/// Loads the kernel into the provided memory.
	pub fn load_kernel(&self, memory: &mut [MaybeUninit<u8>]) -> LoadInfo {
		loaderlog!("Loading kernel to {memory:p}");

		assert!(memory.len() >= self.mem_size());

		let load_start_addr = self
			.phs
			.iter()
			.find(|ph| ph.p_type == program_header::PT_LOAD)
			.unwrap()
			.p_vaddr;

		// Load program segments
		// Contains TLS initialization image
		self.phs
			.iter()
			.filter(|ph| ph.p_type == program_header::PT_LOAD)
			.for_each(|ph| {
				let ph_memory = {
					let mem_start = (ph.p_vaddr - load_start_addr) as usize;
					let mem_len = ph.p_memsz as usize;
					&mut memory[mem_start..][..mem_len]
				};
				let file_len = ph.p_filesz as usize;
				let ph_file = &self.elf[ph.p_offset as usize..][..file_len];
				MaybeUninit::write_slice(&mut ph_memory[..file_len], ph_file);
				for byte in &mut ph_memory[file_len..] {
					byte.write(0);
				}
			});

		// Perform relocations
		self.relas.iter().for_each(|rela| {
			let kernel_addr = memory.as_ptr() as i64;
			match reloc::r_type(rela.r_info) {
				arch::R_RELATIVE => {
					let relocated = kernel_addr + rela.r_addend;
					MaybeUninit::write_slice(
						&mut memory[rela.r_offset as usize..][..mem::size_of_val(&relocated)],
						&relocated.to_ne_bytes(),
					);
				}
				_ => unreachable!(),
			}
		});

		let tls_info = self
			.phs
			.iter()
			.find(|ph| ph.p_type == program_header::PT_TLS)
			.map(|ph| TlsInfo::new(self.header, ph, memory.as_ptr() as u64));

		let entry_point = {
			let mut entry_point = self.header.e_entry;
			if self.header.e_type == header::ET_DYN {
				entry_point += memory.as_ptr() as u64;
			}
			entry_point
		};

		let elf_location = (self.header.e_type == header::ET_EXEC).then_some(load_start_addr);

		LoadInfo {
			elf_location,
			entry_point,
			tls_info,
		}
	}
}

pub struct LoadInfo {
	pub elf_location: Option<u64>,
	pub entry_point: u64,
	pub tls_info: Option<TlsInfo>,
}

pub struct TlsInfo {
	start: u64,
	filesz: u64,
	memsz: u64,
	align: u64,
}

impl TlsInfo {
	fn new(header: &Header, ph: &ProgramHeader, start_addr: u64) -> Self {
		let mut tls_start = ph.p_vaddr;
		if header.e_type == header::ET_DYN {
			tls_start += start_addr;
		}
		let tls_info = TlsInfo {
			start: tls_start,
			filesz: ph.p_filesz,
			memsz: ph.p_memsz,
			align: ph.p_align,
		};
		let range = tls_info.start as *const ()..(tls_info.start + tls_info.memsz) as *const ();
		let len = tls_info.memsz;
		loaderlog!("TLS is at {range:?} ({len} B)",);
		tls_info
	}

	pub fn insert_into(&self, boot_info: &mut BootInfo) {
		boot_info.tls_start = self.start;
		boot_info.tls_filesz = self.filesz;
		boot_info.tls_memsz = self.memsz;
		boot_info.tls_align = self.align;
	}
}
