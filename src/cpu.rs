//! 65816 emulator. Does not emulate internal memory-mapped registers (these are meant to be
//! provided via an implementation of `AddressSpace`).


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

    fn set_nz(&mut self, val: u16) -> u16 {
        if val == 0 {
            self.set_zero(true);
        } else if val & 0x8000 != 0 {
            self.set_negative(true);
        }

        val
    }

    fn set_nz_8(&mut self, val: u8) -> u8 {
        if val == 0 {
            self.set_zero(true);
        } else if val & 0x80 != 0 {
            self.set_negative(true);
        }

        val
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
    a: u16,
    x: u16,
    y: u16,
    /// Stack pointer
    s: u16,
    /// Data bank register. Bank for all memory accesses.
    dbr: u8,
    /// Program bank register. Opcodes are fetched from this bank.
    pbr: u8,
    /// Direct (page) register. Address offset for all instruction using "direct addressing" mode.
    d: u16,
    /// Program counter. Note that PBR is not changed by the CPU, so code can not span multiple
    /// banks (without manual bank switching).
    pc: u16,
    p: StatusReg,
    emulation: bool,

    pub mem: T,
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
            a: 0,
            x: 0,
            y: 0,
            // High byte set to 1 since we're now in emulation mode
            s: 0x0100,
            // Initialized to 0
            dbr: 0,
            d: 0,
            pbr: 0,
            // Read from RESET vector above
            pc: pc,
            // Acc and index regs start in 8-bit mode, IRQs disabled, CPU in emulation mode
            p: StatusReg(SMALL_ACC_FLAG | SMALL_INDEX_FLAG | IRQ_FLAG),
            emulation: true,

            mem: mem,
        }
    }

    /// Fetches the byte PC points at, then increments PC
    fn fetchb(&mut self) -> u8 {
        let b = self.mem.load(self.pbr, self.pc);
        self.pc = self.pc.wrapping_add(1);
        if self.pc == 0 { warn!("pc overflow") }
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
        self.mem.store(0, self.s, value);
        if self.emulation {
            // stack must stay in 0x01xx
            assert_eq!(self.s & 0xff00, 0x0100);
            let s = self.s as u8;
            if s == 0 { warn!("stack overflow") }
            self.s = (self.s & 0xff00) | s.wrapping_sub(1) as u16;
        } else {
            if self.s == 0 { warn!("stack overflow") }
            self.s = self.s.wrapping_sub(1);
        }
    }

    fn pushw(&mut self, value: u16) {
        // FIXME is high or low pushed first? We'll push high first, since JSR does the same
        let hi = (value >> 8) as u8;
        let lo = value as u8;
        self.pushb(hi);
        self.pushb(lo);
    }

    fn popb(&mut self) -> u8 {
        if self.emulation {
            // stack must stay in 0x01xx
            assert_eq!(self.s & 0xff00, 0x0100);
            let s = self.s as u8;
            if s == 0xff { warn!("stack underflow") }
            self.s = (self.s & 0xff00) | s.wrapping_add(1) as u16;
        } else {
            if self.s == 0xffff { warn!("stack underflow") }
            self.s = self.s.wrapping_add(1);
        }

        self.mem.load(0, self.s)
    }

    fn popw(&mut self) -> u16 {
        // FIXME see pushw. we pop low first, then high.
        let lo = self.popb() as u16;
        let hi = self.popb() as u16;
        (hi << 8) | lo
    }

    /// Enters/exits emulation mode
    fn set_emulation(&mut self, value: bool) {
        if !self.emulation && value {
            // Enter emulation mode

            // Set high byte of stack ptr to 0x01
            self.s = 0x0100 | (self.s & 0xff);
        } else if self.emulation && !value {
            // Leave emulation mode (and enter native mode)
        }
        self.emulation = value;
    }

    fn trace_op(&self, pc: u16, op: &str, am: Option<&AddressingMode>) {
        use log::LogLevel::Trace;
        if !log_enabled!(Trace) { return }

        let opstr = format!("{} {}",
            op,
            am.map(|am| am.format(self)).unwrap_or(String::new())
        );
        trace!("{:02X}:{:04X}  {:14} a:{:04X} x:{:04X} y:{:04X} s:{:04X} d:{:02X} dbr:{:02X} pbr:{:02X} emu:{} p:{:08b}",
            self.pbr,
            pc,
            opstr,
            self.a,
            self.x,
            self.y,
            self.s,
            self.d,
            self.dbr,
            self.pbr,
            self.emulation as u8,
            self.p.0,
        );
    }

    /// Executes a single opcode and returns the number of master clock cycles spent doing that.
    pub fn dispatch(&mut self) -> u8 {
        // CPU cycles each opcode takes (not actually that simple)
        static CYCLE_TABLE: [u8; 256] = [
            7,6,7,4,5,3,5,6, 3,2,2,4,6,4,6,5,   // $00 - $0f
            2,5,5,7,5,4,6,6, 2,4,2,2,6,4,7,5,   // $10 - $1f
            6,6,8,4,3,3,5,6, 4,2,2,5,4,4,6,5,   // $20 - $2f
            2,5,5,7,4,4,6,6, 2,4,2,2,4,4,7,5,   // $30 - $3f
            7,6,2,4,7,3,5,6, 3,2,2,3,3,4,6,5,   // $40 - $4f
            2,5,5,7,7,4,6,6, 2,4,3,2,4,4,7,5,   // $50 - $5f
            7,6,6,4,3,3,5,6, 4,2,2,6,5,4,6,5,   // $60 - $6f
            2,5,5,7,4,4,6,6, 2,4,4,2,6,2,7,5,   // $70 - $7f
            2,6,3,4,3,3,3,2, 2,2,2,3,4,4,4,5,   // $80 - $8f
            2,6,5,7,4,4,4,6, 2,5,2,2,3,5,5,5,   // $90 - $9f
            2,6,2,4,3,3,3,6, 2,2,2,4,4,4,4,5,   // $a0 - $af
            2,5,5,7,4,4,4,6, 2,4,2,2,4,4,4,5,   // $b0 - $bf
            2,6,3,4,3,3,5,6, 2,2,2,3,4,4,6,5,   // $c0 - $cf
            2,5,5,7,6,4,6,6, 2,4,3,3,6,4,7,5,   // $d0 - $df
            2,6,3,4,3,3,5,6, 2,2,2,3,4,4,6,5,   // $e0 - $ef
            2,5,5,7,5,4,6,6, 2,4,4,2,6,4,7,5,   // $f0 - $ff
        ];

        let pc = self.pc;

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

        let op = self.fetchb();
        match op {
            // Stack operations
            0x08 => instr!(php),
            0x28 => instr!(plp),
            0x48 => instr!(pha),
            0x68 => instr!(pla),

            // Processor status
            0x18 => instr!(clc),
            0x58 => instr!(cli),
            0x78 => instr!(sei),
            0xfb => instr!(xce),
            0xc2 => instr!(rep immediate8),
            0xe2 => instr!(sep immediate8),

            // Arithmetic
            0x2a => instr!(rol_a),
            0x2f => instr!(and absolute_long),
            0x69 => instr!(adc immediate_acc),
            0xc8 => instr!(iny),

            // Register and memory transfers
            0x5b => instr!(tcd),
            0x1b => instr!(tcs),
            0xaa => instr!(tax),
            0x85 => instr!(sta direct),
            0x8d => instr!(sta absolute),
            0x9d => instr!(sta absolute_indexed_x),
            0x9c => instr!(stz absolute),
            0xa9 => instr!(lda immediate_acc),
            0xb7 => instr!(lda indirect_long_idx),
            0xa2 => instr!(ldx immediate_index),
            0xa0 => instr!(ldy immediate_index),
            0xac => instr!(ldy absolute),

            // Comparisons and control flow
            0xcd => instr!(cmp absolute),
            0xe0 => instr!(cpx immediate_index),
            0x80 => instr!(bra rel),
            0xd0 => instr!(bne rel),
            0x70 => instr!(bvs rel),
            0x20 => instr!(jsr absolute),
            0x60 => instr!(rts),
            _ => {
                instr!(ill);
                panic!("illegal CPU opcode: {:02X}", op);
            }
        }

        // Return master clock cycles used
        CYCLE_TABLE[op as usize] * 6
    }

    /// Common method for all comparison opcodes. Compares `a` to `b` by effectively computing
    /// `a-b`. This method only works correctly for 16-bit values.
    ///
    /// The Z flag is set if both numbers are equal.
    /// The C flag will be set to `a >= b`.
    /// The N flag is set to the most significant bit of `a-b`.
    fn compare(&mut self, a: u16, b: u16) {
        self.p.set_zero(a == b);
        self.p.set_carry(a >= b);
        self.p.set_negative(a.wrapping_sub(b) & 0x8000 != 0);
    }

    /// Does the exact same thing as `compare`, but for 8-bit operands
    fn compare8(&mut self, a: u8, b: u8) {
        self.p.set_zero(a == b);
        self.p.set_carry(a >= b);
        self.p.set_negative(a.wrapping_sub(b) & 0x80 != 0);
    }

    /// Branch to an absolute address
    fn branch(&mut self, target: (u8, u16)) {
        self.pbr = target.0;
        self.pc = target.1;
    }
}

/// Opcode implementations
impl<T: AddressSpace> Cpu<T> {
    /// Pull Processor Status Register
    fn plp(&mut self) {
        let p = self.popb();
        self.p.0 = p;
    }

    /// AND Accumulator with Memory (or immediate)
    fn and(&mut self, am: AddressingMode) {
        if self.p.small_acc() {
            let val = am.loadb(self);
            let res = self.a as u8 & val;
            self.p.set_nz_8(res);
            self.a = (self.a & 0xff00) | res as u16;
        } else {
            let val = am.loadw(self);
            let res = self.a & val;
            self.a = self.p.set_nz(res);
        }
    }

    /// Add With Carry
    fn adc(&mut self, am: AddressingMode) {
        // Sets N, V, C and Z
        // FIXME is this correct? double-check this!
        let c = if self.p.carry() { 1 } else { 0 };
        if self.p.small_acc() {
            let a = self.a as u8;
            let val = am.loadb(self);
            let res = a as u16 + val as u16 + c;
            self.p.set_carry(res > 255);
            let res = res as u8;
            self.p.set_overflow((a ^ val) & 0x80 == 0 && (a ^ res) & 0x80 == 0x80);

            self.a = (self.a & 0xff00) | res as u16;
        } else {
            let a = self.a;
            let val = am.loadw(self);
            let res = a as u32 + val as u32 + c as u32;
            self.p.set_carry(res > 65535);
            let res = res as u16;
            self.p.set_overflow((a ^ val) & 0x8000 == 0 && (a ^ res) & 0x8000 == 0x8000);

            self.a = res;
        }
    }

    /// Rotate Accumulator Left
    fn rol_a(&mut self) {
        // Sets N, Z, and C
        if self.p.small_acc() {
            let a = self.a as u8;
            self.p.set_carry(self.a & 0x80 != 0);
            self.a = (self.a & 0xff00) | self.p.set_nz_8(a.rotate_left(1)) as u16;
        } else {
            self.p.set_carry(self.a & 0x8000 != 0);
            self.a = self.p.set_nz(self.a.rotate_left(1));
        }
    }

    /// Transfer Accumulator to Index Register X
    fn tax(&mut self) {
        // Changes N and Z
        let a = if self.p.small_acc() {
            self.a & 0xff
        } else {
            self.a
        };

        if self.p.small_index() {
            self.x = (self.x & 0xff00) | self.p.set_nz_8(a as u8) as u16;
        } else {
            self.x = self.p.set_nz(a);
        }
    }

    /// Increment Index Register Y
    fn iny(&mut self) {
        // Changes N and Z (XXX really?)
        if self.p.small_index() {
            let res = self.p.set_nz_8((self.y as u8).wrapping_add(1));
            self.y = (self.y & 0xff00) | res as u16;
        } else {
            self.y = self.p.set_nz(self.y.wrapping_add(1));
        }
    }

    /// Push A on the stack
    fn pha(&mut self) {
        // No flags modified
        if self.p.small_acc() {
            let a = self.a as u8;
            self.pushb(a);
        } else {
            let a = self.a;
            self.pushw(a);
        }
    }

    /// Pull Accumulator from stack
    fn pla(&mut self) {
        // Changes N and Z
        if self.p.small_acc() {
            let a = self.popb();
            self.a = (self.a & 0xff00) | self.p.set_nz_8(a) as u16;
        } else {
            let a = self.popw();
            self.a = self.p.set_nz(a);
        }
    }

    /// Branch if Overflow Set
    fn bvs(&mut self, am: AddressingMode) {
        // Changes no flags
        if self.p.overflow() {
            let a = am.address(self);
            self.branch(a);
        }
    }

    /// Branch always
    fn bra(&mut self, am: AddressingMode) {
        // Changes no flags
        let a = am.address(self);
        self.branch(a);
    }

    /// Branch if Not Equal (Branch if Z = 0)
    fn bne(&mut self, am: AddressingMode) {
        // Changes no flags
        if !self.p.zero() {
            let a = am.address(self);
            self.branch(a);
        }
    }

    /// Compare Accumulator with Memory
    fn cmp(&mut self, am: AddressingMode) {
        if self.p.small_acc() {
            let a = self.a as u8;
            let b = am.loadb(self);
            self.compare8(a, b);
        } else {
            let a = self.a;
            let b = am.loadw(self);
            self.compare(a, b);
        }
    }

    /// Compare Index Register X with Memory
    fn cpx(&mut self, am: AddressingMode) {
        if self.p.small_index() {
            let val = am.loadb(self);
            let x = self.x as u8;
            self.compare8(x, val);
        } else {
            let val = am.loadw(self);
            let x = self.x;
            self.compare(x, val);
        }
    }

    /// Push Processor Status Register
    fn php(&mut self) {
        // Changes no flags
        let p = self.p.0;
        self.pushb(p);
    }

    /// Return from Subroutine
    fn rts(&mut self) {
        let pcl = self.popb() as u16;
        let pch = self.popb() as u16;
        let pbr = self.popb();
        self.pbr = pbr;
        self.pc = (pch << 8) | pcl;
    }

    /// Jump to Subroutine
    fn jsr(&mut self, am: AddressingMode) {
        // Changes no flags

        // UGH!!! Come on borrowck, you're supposed to *help*!
        let pbr = self.pbr;
        self.pushb(pbr);
        let pch = (self.pc >> 8) as u8;
        self.pushb(pch);
        let pcl = self.pc as u8;
        self.pushb(pcl);

        // JSR can't immediate. Absolute is handled by storing the address, not the value, in PC.
        self.pc = am.address(self).1;
    }

    /// Clear Interrupt Disable Flag (Enable IRQs)
    fn cli(&mut self) {
        self.p.set_irq_disable(false);
    }

    /// Disable IRQs
    fn sei(&mut self) {
        self.p.set_irq_disable(true);
    }

    /// Store 0 to memory
    fn stz(&mut self, am: AddressingMode) {
        // Changes no flags
        am.storeb(self, 0);
    }

    /// Load accumulator from memory
    fn lda(&mut self, am: AddressingMode) {
        // Changes N and Z
        if self.p.small_acc() {
            let val = am.loadb(self);
            self.a = (self.a & 0xff00) | self.p.set_nz_8(val) as u16;
        } else {
            let val = am.loadw(self);
            self.a = self.p.set_nz(val);
        }
    }

    /// Load Y register from memory
    fn ldy(&mut self, am: AddressingMode) {
        // Changes N and Z
        if self.p.small_index() {
            let val = am.loadb(self);
            self.y = (self.y & 0xff00) | self.p.set_nz_8(val) as u16;
        } else {
            let val = am.loadw(self);
            self.y = self.p.set_nz(val);
        }
    }

    fn ldx(&mut self, am: AddressingMode) {
        // Changes N and Z
        if self.p.small_index() {
            let val = am.loadb(self);
            self.x = (self.x & 0xff00) | self.p.set_nz_8(val) as u16;
        } else {
            let val = am.loadw(self);
            self.x = self.p.set_nz(val);
        }
    }

    /// Store accumulator to memory
    fn sta(&mut self, am: AddressingMode) {
        // Changes no flags
        if self.p.small_acc() {
            let b = self.a as u8;
            am.storeb(self, b);
        } else {
            let w = self.a;
            am.storew(self, w);
        }
    }

    /// Clear carry
    fn clc(&mut self) {
        self.p.set_carry(false);
    }

    /// Exchange carry and emulation flags
    fn xce(&mut self) {
        // FIXME The Wiki says this also changes the M and X flag, what's up with that?
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
        self.d = self.p.set_nz(self.a);
    }

    /// Transfer 16-bit Accumulator to Stack Pointer
    fn tcs(&mut self) {
        self.s = self.a;
    }

    fn ill(&mut self) {}
}

enum AddressingMode {
    Immediate(u16),
    Immediate8(u8),
    /// Access absolute offset in the current data bank
    /// (DBR, <val>)
    Absolute(u16),
    /// Access absolute offset in the specified data bank (DBR is not changed)
    /// (<val0>, <val1>)
    AbsoluteLong(u8, u16),
    /// (DBR, <val> + X)
    AbsIndexedX(u16),
    /// <val> + direct page register in bank 0
    /// (0, D + <val>)
    Direct(u8),
    /// PC-relative, used for jumps
    /// (PBR, PC + <val>)
    Rel(i8),
    /// "Direct Indirect Indexed Long [d],y"
    /// (0, D + <val> + Y)
    IndirectLongIdx(u8),
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
                // ^ if this should be supported, make sure to fix the potential overflow below

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
        use cpu::AddressingMode::*;

        // FIXME is something here dependant on register sizes?
        // FIXME Overflow unclear, use next bank or not? (Probably yes, but let's crash first)

        match *self {
            Absolute(addr) => {
                (cpu.dbr, addr)
            }
            AbsoluteLong(bank, addr) => {
                (bank, addr)
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
            IndirectLongIdx(offset) => {
                let addr = cpu.d + offset as u16 + cpu.y;
                (0, addr)
            }
            Immediate(_) | Immediate8(_) =>
                panic!("attempted to take the address of an immediate value (attempted store to \
                    immediate?)")
        }
    }

    fn format<T: AddressSpace>(&self, cpu: &Cpu<T>) -> String {
        use cpu::AddressingMode::*;

        match *self {
            Immediate(val) => format!("#${:04X}", val),
            Immediate8(val) => format!("#${:02X}", val),
            Absolute(addr) => format!("${:04X}", addr),
            AbsoluteLong(bank, addr) => format!("${:02X}:{:04X}", bank, addr),
            AbsIndexedX(offset) => format!("${:04X},x", offset),
            Rel(rel) => format!("{:+}", rel),
            Direct(offset) => format!("${:02X}", offset),
            IndirectLongIdx(offset) => format!("[${:02X}],y", offset),
        }
    }
}

/// Addressing mode construction
impl<T: AddressSpace> Cpu<T> {
    fn indirect_long_idx(&mut self) -> AddressingMode {
        AddressingMode::IndirectLongIdx(self.fetchb())
    }
    fn absolute(&mut self) -> AddressingMode {
        AddressingMode::Absolute(self.fetchw())
    }
    fn absolute_long(&mut self) -> AddressingMode {
        let addr = self.fetchw();
        let bank = self.fetchb();
        AddressingMode::AbsoluteLong(bank, addr)
    }
    fn absolute_indexed_x(&mut self) -> AddressingMode {
        AddressingMode::AbsIndexedX(self.fetchw())
    }
    fn rel(&mut self) -> AddressingMode {
        AddressingMode::Rel(self.fetchb() as i8)
    }
    fn direct(&mut self) -> AddressingMode {
        AddressingMode::Direct(self.fetchb())
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
