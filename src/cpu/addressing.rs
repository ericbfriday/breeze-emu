//! Contains addressing mode definitions

use cpu::Cpu;

/// As a safety measure, the load and store methods take the mode by value and consume it. Using
/// the same object twice requires an explicit `.clone()` (`Copy` isn't implemented).
#[derive(Clone)]
pub enum AddressingMode {
    Immediate(u16),
    Immediate8(u8),
    /// "Absolute-a"
    /// Access absolute offset in the current data bank
    /// (DBR, <val>)
    Absolute(u16),

    // "Absolute Indexed Indirect-(a,x)"

    /// "Absolute Indexed with X-a,x"
    /// (DBR, <val> + X)
    AbsIndexedX(u16),

    // "Absolute Indexed with Y-a,y"
    // "Absolute Indirect-(a)" (PC?)

    /// "Absolute Long Indexed With X-al,x" - Absolute Long + X
    /// (<val0>, <val1> + X)
    AbsLongIndexedX(u8, u16),

    /// "Absolute Long-al"
    /// Access absolute offset in the specified data bank (DBR is not changed)
    /// (<val0>, <val1>)
    AbsoluteLong(u8, u16),
    /// "Direct-d"
    /// <val> + direct page register in bank 0
    /// (0, D + <val>)
    Direct(u8),
    /// "Direct Indexed with X-d,x"
    /// (0, D + <val> + X)
    DirectIndexedX(u8),
    /// "Program Counter Relative-r"
    /// Used for jumps
    /// (PBR, PC + <val>)  [PC+<val> wraps inside the bank]
    Rel(i8),

    // "Direct Indirect Indexed-(d),y" - Indirect-Y
    // (DBR, D + <val> + Y)  [D+<val> wraps]

    /// "Direct Indirect Indexed Long/Long Indexed-[d],y"
    /// (bank, addr) := load(D + <val>)
    /// (bank, addr + Y)
    IndirectLongIdx(u8),

    // "Direct Indirect Long-[d]"
    // (0, D + <val>)
}

impl AddressingMode {
    /// Loads a byte from where this AM points to (or returns the immediate value)
    pub fn loadb(self, cpu: &mut Cpu) -> u8 {
        match self {
            AddressingMode::Immediate(val) =>
                panic!("loadb on 16-bit immediate (was this intentional?)"),
            AddressingMode::Immediate8(val) => val,
            _ => {
                let (bank, addr) = self.address(cpu);
                cpu.loadb(bank, addr)
            }
        }
    }

    pub fn loadw(self, cpu: &mut Cpu) -> u16 {
        match self {
            AddressingMode::Immediate(val) => val,
            AddressingMode::Immediate8(val) => panic!("loadw on 8-bit immediate"),
            _ => {
                let (bank, addr) = self.address(cpu);
                assert!(addr < 0xffff, "loadw on bank boundary");
                // ^ if this should be supported, make sure to fix the potential overflow below

                let lo = cpu.loadb(bank, addr) as u16;
                let hi = cpu.loadb(bank, addr + 1) as u16;

                (hi << 8) | lo
            }
        }
    }

    pub fn storeb(self, cpu: &mut Cpu, value: u8) {
        let (bank, addr) = self.address(cpu);
        cpu.storeb(bank, addr, value);
    }

    pub fn storew(self, cpu: &mut Cpu, value: u16) {
        let (bank, addr) = self.address(cpu);
        assert!(addr < 0xffff, "storew on bank boundary");

        cpu.storeb(bank, addr, value as u8);
        cpu.storeb(bank, addr + 1, (value >> 8) as u8);
    }

    /// Computes the effective address as a bank-address-tuple. Panics if the addressing mode is
    /// immediate.
    pub fn address(&self, cpu: &mut Cpu) -> (u8, u16) {
        use self::AddressingMode::*;

        // FIXME is something here dependant on register sizes?
        // FIXME Overflow unclear, use next bank or not? (Probably yes, but let's crash first)

        match *self {
            Absolute(addr) => {
                (cpu.dbr, addr)
            }
            AbsoluteLong(bank, addr) => {
                (bank, addr)
            }
            AbsLongIndexedX(bank, addr) => {
                let a = ((bank as u32) << 16) | addr as u32;
                let eff_addr = a + cpu.x as u32;
                assert!(eff_addr & 0xff000000 == 0, "address overflow");
                let bank = eff_addr >> 16;
                let addr = eff_addr as u16;
                (bank as u8, addr)
            }
            AbsIndexedX(offset) => {
                (cpu.dbr, offset + cpu.x)
            }
            Rel(rel) => {
                (cpu.pbr, (cpu.pc as i32 + rel as i32) as u16)
            }
            Direct(offset) => {
                (0, cpu.d.wrapping_add(offset as u16))
            }
            DirectIndexedX(offset) => {
                (0, cpu.d.wrapping_add(offset as u16).wrapping_add(cpu.x))
            }
            IndirectLongIdx(offset) => {
                // "The 24-bit base address is pointed to by the sum of the second byte of the
                // instruction and the Direct Register. The effective address is this 24-bit base
                // address plus the Y Index Register."
                let addr_ptr = cpu.d.wrapping_add(offset as u16);
                let lo = cpu.loadb(0, addr_ptr) as u32;
                let hi = cpu.loadb(0, addr_ptr + 1) as u32;
                let bank = cpu.loadb(0, addr_ptr + 2) as u32;
                let base_address = (bank << 16) | (hi << 8) | lo;
                let eff_addr = base_address + cpu.y as u32;
                assert!(eff_addr & 0xff000000 == 0, "address overflow");

                let bank = (eff_addr >> 16) as u8;
                let addr = eff_addr as u16;
                (bank, addr)
            }
            Immediate(_) | Immediate8(_) =>
                panic!("attempted to take the address of an immediate value (attempted store to \
                    immediate?)")
        }
    }

    pub fn format(&self, cpu: &Cpu) -> String {
        use self::AddressingMode::*;

        match *self {
            Immediate(val) =>              format!("#${:04X}", val),
            Immediate8(val) =>             format!("#${:02X}", val),
            Absolute(addr) =>              format!("${:04X}", addr),
            AbsoluteLong(bank, addr) =>    format!("${:02X}:{:04X}", bank, addr),
            AbsLongIndexedX(bank, addr) => format!("${:02X}:{:04X},x", bank, addr),
            AbsIndexedX(offset) =>         format!("${:04X},x", offset),
            Rel(rel) =>                    format!("{:+}", rel),
            Direct(offset) =>              format!("${:02X}", offset),
            DirectIndexedX(offset) =>      format!("${:02X},x", offset),
            IndirectLongIdx(offset) =>     format!("[${:02X}],y", offset),
        }
    }
}
