//! 65816 emulator. Does not emulate internal memory-mapped registers (these are meant to be
//! provided via an implementation of `AddressSpace`).

use std::num::Wrapping as W;

pub type U8 = W<u8>;
pub type U16 = W<u16>;
pub type U32 = W<u32>;

/// Abstraction over memory operations executed by the CPU. If these operations access an unmapped
/// address, the methods in here will be used to perform the operation.
pub trait AddressSpace {
    /// Load a byte from the given address.
    fn load(&mut self, bank: u8, addr: u16) -> u8;

    /// Store a byte at the given address.
    fn store(&mut self, bank: u8, addr: u16, value: u8);
}

const NEG_FLAG: u8 = 0x80;
const OVERFLOW_FLAG: u8 = 0x40;
/// 1 = Accumulator is 8-bit (native mode only)
const SMALL_ACC_FLAG: u8 = 0x20;
/// 1 = Index registers X/Y are 8-bit (native mode only)
const SMALL_INDEX_FLAG: u8 = 0x10;
/// Emulation mode only (same bit as `SMALL_INDEX_FLAG`)
const BREAK_FLAG: u8 = 0x10;
const DEC_FLAG: u8 = 0x08;
/// 1 = IRQs disabled
const IRQ_FLAG: u8 = 0x04;
const ZERO_FLAG: u8 = 0x02;
const CARRY_FLAG: u8 = 0x01;
struct StatusReg(u8);

impl StatusReg {
    fn negative(&self) -> bool    { self.0 & NEG_FLAG != 0 }
    fn overflow(&self) -> bool    { self.0 & OVERFLOW_FLAG != 0 }
    fn zero(&self) -> bool        { self.0 & ZERO_FLAG != 0}
    fn carry(&self) -> bool       { self.0 & CARRY_FLAG != 0 }
    fn irq_disable(&self) -> bool { self.0 & IRQ_FLAG != 0 }
    fn small_acc(&self) -> bool   { self.0 & SMALL_ACC_FLAG != 0 }
    fn small_index(&self) -> bool { self.0 & SMALL_INDEX_FLAG != 0 }

    fn set(&mut self, flag: u8, value: bool) {
        if value {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }

    fn set_negative(&mut self, value: bool)    { self.set(NEG_FLAG, value) }
    fn set_overflow(&mut self, value: bool)    { self.set(OVERFLOW_FLAG, value) }
    fn set_zero(&mut self, value: bool)        { self.set(ZERO_FLAG, value) }
    fn set_carry(&mut self, value: bool)       { self.set(CARRY_FLAG, value) }
    fn set_irq_disable(&mut self, value: bool) { self.set(IRQ_FLAG, value) }

    fn set_nz(&mut self, value: u16) -> u16 {
        if value == 0 {
            self.set_zero(true);
        } else if value & 0x8000 != 0 {
            self.set_negative(true);
        }

        value
    }
}

// Emulation mode vectors
const IRQ_VEC8: u16 = 0xFFFE;
const RESET_VEC8: u16 = 0xFFFC;
const NMI_VEC8: u16 = 0xFFFA;
const ABORT_VEC8: u16 = 0xFFF8;
const COP_VEC8: u16 = 0xFFF4;

// Native mode vectors
const IRQ_VEC16: u16 = 0xFFEE;
const NMI_VEC16: u16 = 0xFFEA;
const ABORT_VEC16: u16 = 0xFFE8;
const BRK_VEC16: u16 = 0xFFE6;
const COP_VEC16: u16 = 0xFFE4;

pub struct Cpu<T: AddressSpace> {
    a: U16,
    x: U16,
    y: U16,
    /// Stack pointer
    s: U16,
    /// Data bank register. Bank for all memory accesses.
    dbr: U8,
    /// Program bank register. Opcodes are fetched from this bank.
    pbr: U8,
    /// Direct (page) register. Address offset for all instruction using "direct addressing" mode.
    d: U16,
    /// Program counter. Note that PBR is not changed by the CPU, so code can not span multiple
    /// banks (without manual bank switching).
    pc: U16,
    p: StatusReg,
    emulation: bool,

    mem: T,
}

impl<T: AddressSpace> Cpu<T> {
    /// Creates a new CPU and executes a reset. This will fetch the RESET vector from memory and
    /// put the CPU in emulation mode.
    pub fn new(mut mem: T) -> Cpu<T> {
        let pcl = mem.load(0, RESET_VEC8) as u16;
        let pch = mem.load(0, RESET_VEC8 + 1) as u16;
        let pc = (pch << 8) | pcl;
        debug!("RESET @ {:02X}", pc);

        Cpu {
            // Undefined according to datasheet
            a: W(0),
            x: W(0),
            y: W(0),
            // High byte set to 1 since we're now in emulation mode
            s: W(0x0100),
            // Initialized to 0
            dbr: W(0),
            d: W(0),
            pbr: W(0),
            // Read from RESET vector above
            pc: W(pc),
            // Acc and index regs start in 8-bit mode, IRQs disabled, CPU in emulation mode
            p: StatusReg(SMALL_ACC_FLAG | SMALL_INDEX_FLAG | IRQ_FLAG),
            emulation: true,

            mem: mem,
        }
    }

    /// Fetches the byte PC points at, then increments PC
    fn fetchb(&mut self) -> u8 {
        let b = self.mem.load(self.pbr.0, self.pc.0);
        self.pc = self.pc + W(1);
        b
    }

    /// Fetches a 16-bit word (little-endian) located at PC, by fetching 2 individual bytes
    fn fetchw(&mut self) -> u16 {
        let low = self.fetchb() as u16;
        let high = self.fetchb() as u16;
        (high << 8) | low
    }

    /// Pushes a byte onto the stack and decrements the stack pointer
    fn pushb(&mut self, value: u8) {
        self.mem.store(0, self.s.0, value);
        self.s = self.s - W(1);
    }

    /// Enters/exits emulation mode
    fn set_emulation(&mut self, value: bool) {
        if !self.emulation && value {
            // Enter emulation mode

            // Set high byte of stack ptr to 0x01
            self.s.0 = 0x0100 | (self.s.0 & 0xff);
        } else if self.emulation && !value {
            // Leave emulation mode (and enter native mode)
        }
        self.emulation = value;
    }

    fn trace_op(&self, pc: u16, op: &str, am: Option<&AddressingMode>) {
        trace!("{:02X}:{:04X}  {} {:10} a:{:04X} x:{:04X} y:{:04X} s:{:04X} dbr:{:02X} pbr:{:02X} emu:{} p:{:08b}",
            self.pbr.0,
            pc,
            op,
            am.map(|am| am.format(self)).unwrap_or(String::new()),
            self.a.0,
            self.x.0,
            self.y.0,
            self.s.0,
            self.dbr.0,
            self.pbr.0,
            self.emulation as u8,
            self.p.0,
        );
    }

    /// FIXME Temporary function to test the CPU emulation
    pub fn run(&mut self) {
        let mut pc;
        macro_rules! instr {
            ( $name:ident ) => {{
                self.trace_op(pc, stringify!($name), None);
                self.$name()
            }};
            ( $name:ident $am:ident ) => {{
                let am = self.$am();
                self.trace_op(pc, stringify!($name), Some(&am));
                self.$name(am)
            }};
        }

        loop {
            pc = self.pc.0;
            let op = self.fetchb();

            match op {
                0x18 => instr!(clc),
                0x1b => instr!(tcs),
                0x20 => instr!(jsr absolute),
                0x5b => instr!(tcd),
                0x78 => instr!(sei),
                0x8d => instr!(sta absolute),
                0x9c => instr!(stz absolute),
                0xa9 => instr!(lda immediate_acc),
                0xc2 => instr!(rep immediate8),
                0xe2 => instr!(sep immediate8),
                0xfb => instr!(xce),
                _ => panic!("illegal opcode: {:02X}", op),
            }
        }
    }
}

/// Opcode implementations
impl<T: AddressSpace> Cpu<T> {
    /// Jump to Subroutine
    fn jsr(&mut self, am: AddressingMode) {
        // UGH!!! Come on borrowck, you're supposed to *help*!
        let pbr = self.pbr.0;
        self.pushb(pbr);
        let pch = (self.pc.0 >> 8) as u8;
        self.pushb(pch);
        let pcl = self.pc.0 as u8;
        self.pushb(pcl);

        // JSR can't immediate. Absolute is handled by storing the address, not the value, in PC.
        self.pc.0 = am.address(self).1;
    }

    /// Disable IRQs
    fn sei(&mut self) {
        self.p.set_irq_disable(true);
    }

    /// Store 0 to memory
    fn stz(&mut self, am: AddressingMode) {
        am.storeb(self, 0);
    }

    /// Load accumulator from memory
    fn lda(&mut self, am: AddressingMode) {
        if self.p.small_acc() {
            self.a.0 = self.a.0 & 0xff00;
        } else {
            self.a.0 = am.loadw(self);
        }

        // XXX is this correct (use 16-bit value in all cases)?
        self.p.set_nz(self.a.0);
    }

    /// Store accumulator to memory
    fn sta(&mut self, am: AddressingMode) {
        if self.p.small_acc() {
            let b = self.a.0 as u8;
            am.storeb(self, b);
        } else {
            let w = self.a.0;
            am.storew(self, w);
        }
    }

    /// Clear carry
    fn clc(&mut self) {
        self.p.set_carry(false);
    }

    /// Exchange carry and emulation flags
    fn xce(&mut self) {
        let carry = self.p.carry();
        let e = self.emulation;
        self.p.set_carry(e);
        self.set_emulation(carry);
    }

    /// Reset status bits
    ///
    /// Clears the bits in the status register that are 1 in the argument (argument is interpreted
    /// as 8-bit)
    fn rep(&mut self, am: AddressingMode) {
        assert!(!self.emulation);
        self.p.0 &= !am.loadb(self);
    }

    /// Set Processor Status Bits
    fn sep(&mut self, am: AddressingMode) {
        assert!(!self.emulation);
        self.p.0 |= am.loadb(self);
    }

    /// Transfer 16-bit Accumulator to Direct Page Register
    fn tcd(&mut self) {
        self.d.0 = self.p.set_nz(self.a.0);
    }

    /// Transfer 16-bit Accumulator to Stack Pointer
    fn tcs(&mut self) {
        self.s = self.a;
    }
}

enum AddressingMode {
    Absolute(u16),
    Immediate(u16),
    Immediate8(u8),
}

impl AddressingMode {
    /// Loads a byte from where this AM points to (or returns the immediate value)
    fn loadb<T: AddressSpace>(self, cpu: &mut Cpu<T>) -> u8 {
        match self {
            AddressingMode::Immediate(val) =>
                panic!("loadb on 16-bit immediate (was this intentional?)"),
            AddressingMode::Immediate8(val) => val,
            _ => {
                let (bank, addr) = self.address(cpu);
                cpu.mem.load(bank, addr)
            }
        }
    }

    fn loadw<T: AddressSpace>(self, cpu: &mut Cpu<T>) -> u16 {
        match self {
            AddressingMode::Immediate(val) => val,
            AddressingMode::Immediate8(val) => panic!("loadw on 8-bit immediate"),
            _ => {
                let (bank, addr) = self.address(cpu);
                assert!(addr < 0xffff, "loadw on bank boundary");

                let lo = cpu.mem.load(bank, addr) as u16;
                let hi = cpu.mem.load(bank, addr + 1) as u16;

                (hi << 8) | lo
            }
        }
    }

    fn storeb<T: AddressSpace>(self, cpu: &mut Cpu<T>, value: u8) {
        let (bank, addr) = self.address(cpu);
        cpu.mem.store(bank, addr, value);
    }

    fn storew<T: AddressSpace>(self, cpu: &mut Cpu<T>, value: u16) {
        let (bank, addr) = self.address(cpu);
        assert!(addr < 0xffff, "loadw on bank boundary");

        cpu.mem.store(bank, addr, value as u8);
        cpu.mem.store(bank, addr, (value >> 8) as u8);
    }

    /// Computes the effective address as a bank-address-tuple. Panics if the addressing mode is
    /// immediate.
    fn address<T: AddressSpace>(&self, cpu: &Cpu<T>) -> (u8, u16) {
        match *self {
            AddressingMode::Absolute(addr) => {
                (cpu.dbr.0, addr)
            }
            AddressingMode::Immediate(_) | AddressingMode::Immediate8(_) =>
                panic!("attempted to take the address of an immediate value (attempted store to \
                    immediate?)")
        }
    }

    fn format<T: AddressSpace>(&self, cpu: &Cpu<T>) -> String {
        match *self {
            AddressingMode::Absolute(addr) => format!("${:04X}", addr),
            AddressingMode::Immediate(val) => format!("#${:04X}", val),
            AddressingMode::Immediate8(val) => format!("#${:02X}", val),
        }
    }
}

/// Addressing mode construction
impl<T: AddressSpace> Cpu<T> {
    fn absolute(&mut self) -> AddressingMode {
        AddressingMode::Absolute(self.fetchw())
    }
    /// Immediate value with accumulator size
    fn immediate_acc(&mut self) -> AddressingMode {
        if self.p.small_acc() {
            AddressingMode::Immediate8(self.fetchb())
        } else {
            AddressingMode::Immediate(self.fetchw())
        }
    }
    /// Immediate value with index register size
    fn immediate_index(&mut self) -> AddressingMode {
        if self.p.small_index() {
            AddressingMode::Immediate8(self.fetchb())
        } else {
            AddressingMode::Immediate(self.fetchw())
        }
    }
    /// Immediate value, one byte
    fn immediate8(&mut self) -> AddressingMode {
        AddressingMode::Immediate8(self.fetchb())
    }
}