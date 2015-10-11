//! PPU rendering code

use super::{Ppu, Rgb};

use arrayvec::ArrayVec;
use std::mem::replace;

/// "Persistent" render state stored inside the `Ppu`.
#[derive(Default)]
pub struct RenderState {
    /// Contains up to 34 `SpriteTile`s that are visible on the current scanline
    visible_sprite_tiles: Vec<SpriteTile>,
}

/// Unpacked OAM entry for internal use.
struct OamEntry {
    /// First tile (0-255), needs to take name table selection bit into account
    tile: u8,
    /// Sprite's name table (0 or 1).
    name_table: u8,
    /// 9 bits, considered signed (-256 - 255)
    x: i16,
    y: u8,
    /// 0-3
    priority: u8,
    /// 0-7. The first palette entry is `128+ppp*16`.
    palette: u8,
    hflip: bool,
    vflip: bool,
    size_toggle: bool,
}

/// Informations about a single tile of a sprite, needed for drawing.
struct SpriteTile {
    /// Address of character data for this tile
    chr_addr: u16,
    /// X position of the tile on the screen.
    x: i16,
    /// Y position of the scanline inside the tile (0-7)
    /// FIXME Can we just store the pixel row that's on the scanline? (for each priority)
    y_off: u8,
    /// Priority of the sprite (0-3)
    priority: u8,
    /// Palette of the sprite (0-7)
    palette: u8,

    // FIXME hflip/vflip
}

/// Collected background settings
struct BgSettings {
    /// Mosaic pixel size (1-16). 1 = Normal pixels.
    /// FIXME: I think there's a difference between disabled and enabled with 1x1 mosaic size in
    /// some modes (highres presumably)
    mosaic: u8,
    /// Tilemap address in VRAM
    /// "Starting at the tilemap address, the first $800 bytes are for tilemap A. Then come the
    /// $800 bytes for B, then C then D."
    tilemap_addr: u16,
    /// When `true`, this BGs tilemaps are mirrored sideways
    tilemap_mirror_h: bool,
    /// When `true`, this BGs tilemaps are mirrored downwards
    tilemap_mirror_v: bool,
    /// Either 8 or 16.
    tile_size: u8,
    /// Character Data / Tileset address in VRAM
    chr_addr: u16,
    hscroll: u16,
    vscroll: u16,
}

/// Unpacked tilemap entry for internal (rendering) use
struct TilemapEntry {
    vflip: bool,
    hflip: bool,
    /// Priority bit (0-1)
    priority: u8,
    /// Tile palette (0-7)
    palette: u8,
    /// Index into the character/tile data, where the actual tile is stored
    tile_number: u16,
}

/// Rendering
impl Ppu {
    /// Get the configured sprite size in pixels
    fn obj_size(&self, alt: bool) -> (u8, u8) {
        match self.obsel & 0b111 {
            0b000 => if !alt {(8,8)} else {(16,16)},
            0b001 => if !alt {(8,8)} else {(32,32)},
            0b010 => if !alt {(8,8)} else {(64,64)},
            0b011 => if !alt {(16,16)} else {(32,32)},
            0b100 => if !alt {(16,16)} else {(64,64)},
            0b101 => if !alt {(32,32)} else {(64,64)},
            // FIXME Figure out if we want to support these:
            //0b110 => if !alt {(16,32)} else {(32,64)},
            //0b111 => if !alt {(16,32)} else {(32,32)},
            invalid => panic!("invalid sprite size selected: {:b} (OBSEL = ${:02X})",
                invalid, self.obsel)
        }
    }

    /// Reads the tilemap entry at the given VRAM word address.
    ///     vhopppcc cccccccc (high, low)
    ///     v/h        = Vertical/Horizontal flip this tile.
    ///     o          = Tile priority.
    ///     ppp        = Tile palette. The number of entries in the palette depends on the Mode and the BG.
    ///     cccccccccc = Tile number.
    fn tilemap_entry(&self, word_address: u16) -> TilemapEntry {
        let lo = self.vram[word_address * 2];
        let hi = self.vram[word_address * 2 + 1];

        TilemapEntry {
            vflip: hi & 0x80 != 0,
            hflip: hi & 0x40 != 0,
            priority: (hi & 0x20) >> 5,
            palette: (hi & 0x1c) >> 2,
            tile_number: ((hi as u16 & 0x03) << 8) | lo as u16,
        }
    }

    /// Collects properties of a background layer
    fn bg_settings(&self, bg: u8) -> BgSettings {
        // The BGxSC register for our background layer
        let bgsc = match bg {
            1 => self.bg1sc,
            2 => self.bg2sc,
            3 => self.bg3sc,
            4 => self.bg4sc,
            _ => unreachable!(),
        };
        // Chr address >> 12
        let chr = match bg {
            1 => self.bg12nba & 0x0f,
            2 => (self.bg12nba & 0xf0) >> 4,
            3 => self.bg34nba & 0x0f,
            4 => (self.bg34nba & 0xf0) >> 4,
            _ => unreachable!(),
        };
        let (hscroll, vscroll) = match bg {
            1 => (self.bg1hofs, self.bg1vofs),
            2 => (self.bg2hofs, self.bg2vofs),
            3 => (self.bg3hofs, self.bg3vofs),
            4 => (self.bg4hofs, self.bg4vofs),
            _ => unreachable!(),
        };

        BgSettings {
            mosaic: if self.mosaic & (1 << (bg-1)) == 0 {
                1
            } else {
                ((self.mosaic & 0xf0) >> 4) + 1
            },
            tilemap_addr: ((bgsc as u16 & 0xfc) >> 2) << 10,
            tilemap_mirror_h: bgsc & 0b01 == 0, // inverted bit value
            tilemap_mirror_v: bgsc & 0b10 == 0, // inverted bit value
            tile_size: match self.bg_mode() {
                // "If the BG character size for BG1/BG2/BG3/BG4 bit is set, then the BG is made of
                // 16x16 tiles. Otherwise, 8x8 tiles are used. However, note that Modes 5 and 6
                // always use 16-pixel wide tiles, and Mode 7 always uses 8x8 tiles."
                5 | 6 => 16,
                7 => 8,
                _ => {
                    // BGMODE: `4321----` (`-` = not relevant here)
                    if self.bgmode & (1 << (bg + 3)) == 0 {
                        8
                    } else {
                        16
                    }
                }
            },
            chr_addr: (chr as u16) << 12,
            hscroll: hscroll,
            vscroll: vscroll,
        }
    }

    /// Determines whether the given BG layer is enabled
    fn bg_enabled(&self, bg: u8) -> bool { self.tm & (1 << (bg-1)) != 0 }

    /// Returns the OAM entry of the given sprite. Always returns a valid entry if `index` is valid
    /// (0...127), panics otherwise.
    fn get_oam_entry(&self, index: u8) -> OamEntry {
        // FIXME Is this correct?
        let start = index as u16 * 4;
        let mut x = self.oam[start] as u16;

        // vhoopppN
        let byte4 = self.oam[start + 3];
        let vflip = byte4 & 0x80 != 0;
        let hflip = byte4 & 0x40 != 0;
        let priority = (byte4 & 0x30) >> 4;
        let palette = (byte4 & 0x0e) >> 1;

        // Read the second table. Each byte contains information of 4 sprites (2 bits per sprite):
        // The LSb is the size-toggle bit, the second bit is the MSb of the x coord
        let byte = self.oam[512 + index as u16 / 4];
        let info = (byte >> (index & 0x03)) & 0x03;
        let size_toggle = info & 0x01 != 0;
        if info & 0x02 != 0 {
            // MSb of `x` is set, so `x` is negative. Since `x` is a signed 9-bit value, we have to
            // sign-extend it to 16 bits by setting all bits starting from the MSb to 1.
            x = 0xfff0 | x;
        }

        OamEntry {
            tile: self.oam[start + 2],
            name_table: byte4 & 1,
            x: x as i16,
            y: self.oam[start + 1],
            priority: priority,
            palette: palette,
            hflip: hflip,
            vflip: vflip,
            size_toggle: size_toggle,
        }
    }

    /// Returns the active BG mode (0-7).
    fn bg_mode(&self) -> u8 { self.bgmode & 0b111 }
    /// Returns the backdrop color used as a default color (with color math applied, if enabled).
    fn backdrop_color(&self) -> Rgb {
        // TODO: Color math
        self.lookup_color(0)
    }

    /// Looks up a color index in the CGRAM
    fn lookup_color(&self, color: u8) -> Rgb {
        // FIXME Is this correct?
        // 16-bit big endian value! (high byte, high address first)
        // -bbbbbgg gggrrrrr
        let lo = self.cgram[color as u16 * 2] as u16;
        let hi = self.cgram[color as u16 * 2 + 1] as u16;
        // Unused bit should be 0 (just a sanity check, can be removed if games actually do this)
        debug_assert_eq!(hi & 0x80, 0);

        let val = (hi << 8) | lo;
        let b = (val & 0x7c00) >> 10;
        let g = (val & 0x03e0) >> 5;
        let r = val & 0x001f;
        Rgb { r: (r as u8) << 3, g: (g as u8) << 3, b: (b as u8) << 3 }
    }

    /// Returns the number of colors in the given BG layer in the current BG mode (4, 16, 128 or
    /// 256).
    ///
    ///     Mode    # Colors for BG
    ///     1   2   3   4
    ///     ======---=---=---=---=
    ///     0        4   4   4   4
    ///     1       16  16   4   -
    ///     2       16  16   -   -
    ///     3      256  16   -   -
    ///     4      256   4   -   -
    ///     5       16   4   -   -
    ///     6       16   -   -   -
    ///     7      256   -   -   -
    ///     7EXTBG 256 128   -   -
    fn color_count_for_bg(&self, bg: u8) -> u16 {
        match self.bg_mode() {
            0 => 4,
            1 => match bg {
                1 | 2 => 16,
                3 => 4,
                _ => unreachable!(),
            },
            2 => 16,
            3 => match bg {
                1 => 256,
                2 => 16,
                _ => unreachable!(),
            },
            4 => match bg {
                1 => 256,
                2 => 4,
                _ => unreachable!(),
            },
            5 => match bg {
                1 => 16,
                2 => 4,
                _ => unreachable!(),
            },
            6 => 16,
            7 => panic!("NYI: color_count_for_bg for mode 7"),   // (make sure to handle EXTBG)
            _ => unreachable!(),
        }
    }

    /// Calculates the palette base index for a tile in the given background layer. `tile_palette`
    /// is the palette number stored in the tilemap entry (the 3 `p` bits).
    fn palette_base_for_bg_tile(&self, bg: u8, palette_num: u8) -> u8 {
        match self.bg_mode() {
            0 => palette_num * 4 + (bg - 1) * 32,
            1 | 5 => palette_num * self.color_count_for_bg(bg) as u8,   // doesn't have 256 colors
            2 => palette_num * 16,
            3 => match bg {
                1 => 0,
                2 => palette_num * 16,
                _ => unreachable!(),    // no BG3/4
            },
            4 => match bg {
                1 => 0,
                2 => palette_num * 4,
                _ => unreachable!(),    // no BG3/4
            },
            6 => palette_num * 16,      // BG1 has 16 colors
            7 => panic!("NYI: palette_base_for_bg_tile for mode 7"),
            _ => unreachable!(),
        }
    }

    /// Main rendering entry point. Renders the current pixel and returns its color. Assumes that
    /// we're not in any blank mode.
    pub fn render_pixel(&mut self) -> Rgb {
        if self.x == 0 && self.scanline == 0 {
            trace!("New frame. BG mode {}, layers enabled: {:05b}, sprites are {:?} or {:?}",
                self.bg_mode(),
                self.tm & 0x1f,
                self.obj_size(false),
                self.obj_size(true));
        }

        if self.x == 0 {
            // Entered new scanline.
            self.collect_sprite_data_for_scanline();
        }

        if self.scanline < 64 && self.x < 64 {
            // Debug: Draw the palette
            let x = self.x >> 2;
            let y = self.scanline >> 2;
            return self.lookup_color(y as u8 * 16 + x as u8)
        }

        macro_rules! e {
            ( $e:expr ) => ( $e );
        }

        // This macro gets the current pixel from a tile with given priority in the given layer.
        // If the pixel is non-transparent, it will return its RGB value (after applying color
        // math). If it is transparent, it will do nothing (ie. the code following this macro is
        // executed).
        macro_rules! try_layer {
            ( Sprites with priority $prio:tt ) => {
                if let Some(rgb) = self.maybe_draw_sprite_pixel(e!($prio)) {
                    return rgb
                }
            };
            ( BG $bg:tt tiles with priority $prio:tt ) => {
                if let Some(rgb) = self.lookup_bg_color(e!($bg), e!($prio)) {
                    return rgb
                }
            };
        }

        match self.bg_mode() {
            0 => {
                // I love macros <3
                try_layer!(Sprites with priority 3);
                try_layer!(BG 1 tiles with priority 1);
                try_layer!(BG 2 tiles with priority 1);
                try_layer!(Sprites with priority 2);
                try_layer!(BG 1 tiles with priority 0);
                try_layer!(BG 2 tiles with priority 0);
                try_layer!(Sprites with priority 1);
                try_layer!(BG 3 tiles with priority 1);
                try_layer!(BG 4 tiles with priority 1);
                try_layer!(Sprites with priority 0);
                try_layer!(BG 3 tiles with priority 0);
                try_layer!(BG 4 tiles with priority 0);
                self.backdrop_color()
            }
            1 => {
                if self.bgmode & 0x08 != 0 { try_layer!(BG 3 tiles with priority 1) }
                try_layer!(Sprites with priority 3);
                try_layer!(BG 1 tiles with priority 1);
                try_layer!(BG 2 tiles with priority 1);
                try_layer!(Sprites with priority 2);
                try_layer!(BG 1 tiles with priority 0);
                try_layer!(BG 2 tiles with priority 0);
                try_layer!(Sprites with priority 1);
                if self.bgmode & 0x08 == 0 { try_layer!(BG 3 tiles with priority 1) }
                try_layer!(Sprites with priority 0);
                try_layer!(BG 3 tiles with priority 0);
                self.backdrop_color()
            }
            2 ... 5 => {
                // FIXME Do the background priorities differ here?
                try_layer!(Sprites with priority 3);
                try_layer!(BG 1 tiles with priority 1);
                try_layer!(Sprites with priority 2);
                try_layer!(BG 2 tiles with priority 1);
                try_layer!(Sprites with priority 1);
                try_layer!(BG 1 tiles with priority 0);
                try_layer!(Sprites with priority 0);
                try_layer!(BG 2 tiles with priority 0);
                self.backdrop_color()
            }
            6 => {
                try_layer!(Sprites with priority 3);
                try_layer!(BG 1 tiles with priority 1);
                try_layer!(Sprites with priority 2);
                try_layer!(Sprites with priority 1);
                try_layer!(BG 1 tiles with priority 0);
                try_layer!(Sprites with priority 0);
                self.backdrop_color()
            }
            7 => panic!("NYI: BG mode 7"),
            _ => unreachable!(),
        }
    }

    fn collect_sprite_data_for_scanline(&mut self) {
        // FIXME Determine `FirstSprite` correctly (`$2103` priority rotation)
        let first_sprite = 0;

        // Find the first 32 sprites on the current scanline
        // NB Priority is ignored for this step, it's only used for drawing, which isn't done here
        let mut visible_sprites = ArrayVec::<[_; 32]>::new();
        for i in first_sprite..first_sprite+128 {
            let entry = self.get_oam_entry(i);
            if self.sprite_on_scanline(&entry) {
                trace_unique!(
                    "sprite {} on scanline {}: pos = ({}, {}), size = {:?} palette = {}, \
                    prio = {}, tile0 = {}, nametable = {}",
                    i, self.scanline, entry.x, entry.y, self.obj_size(entry.size_toggle),
                    entry.palette, entry.priority, entry.tile, entry.name_table);

                if let Some(_) = visible_sprites.push(entry) {
                    // FIXME: Sprite overflow. Set bit 6 of $213e.
                    break
                }
            }
        }

        // "Starting with the last sprite in Range, load up to 34 8x8 tiles (from left-to-right,
        // after flipping). If there are more than 34 tiles in Range, set bit 7 of $213e. Only
        // those tiles with -8 < X < 256 are counted."
        // A few notes:
        // * Sprite tiles are always 8x8 pixels
        // * Sprites do not have tile maps like BGs do
        // * "left-to-right" refers to how tiles of sprites are loaded, not the sprite order
        // * Tiles are loaded iff they are on the current scanline (and have `-8 < X < 256`)
        // FIXME Is this ^^ correct?

        // FIXME Use `ArrayVec<[_; 34]>` when it works
        let mut visible_tiles = replace(&mut self.render_state.visible_sprite_tiles, Vec::new());
        visible_tiles.clear();

        let name_base: u16 = self.obsel as u16 & 0b111;
        let name_select: u16 = (self.obsel as u16 >> 3) & 0b11;

        // Start at the last sprite found
        'collect_tiles: for sprite in visible_sprites.iter().rev() {
            // How many tiles are there?
            let (sprite_w, _) = self.obj_size(sprite.size_toggle);
            let sprite_w_tiles = sprite_w / 8;
            // Offset into the sprite
            let sprite_y_off = self.scanline - sprite.y as u16;
            // Tile Y coordinate of the tile row we're interested in (tiles on the scanline)
            let y_tile = sprite_y_off / 8;
            // Y offset into the tile row
            let tile_y_off = (sprite_y_off % 8) as u8;

            // Calculate VRAM word address of first tile. Depends on base/name bits in `$2101`.
            let tile_start_word_addr =
                ((name_base << 13) +
                ((sprite.tile as u16) << 4) +
                (sprite.name_table as u16 * ((name_select + 1) << 12))) & 0x7fff;
            let tile_start_addr = tile_start_word_addr * 2;

            // The character data for the first tile is stored at `tile_start_addr`, in the same
            // format as BG character data (bitplanes, etc.). Keep in mind that sprites do not have
            // tilemaps.
            // One 8x8 tile is 32 Bytes large (4 bits per pixel).
            // Tiles in a single (8 pixel high) row of the sprite are stored sequentially: Tile
            // coord (1,0) is stored directly behind (0,0), which is stored at `tile_start_addr`.
            // Rows of tiles, however, are always stored 512 Bytes (or 16 tiles/128 pixels) apart:
            // If tile (0,0) is at address $0000, tile (0,1) is at $0200. This is independent of
            // the sprite size, which means that there are "holes" in the sprite character data,
            // which are used to store the data of other sprites.

            // Start address of the row of tiles on the scanline
            let y_row_start_addr = tile_start_addr + 512 * y_tile;

            // FIXME "Only those tiles with -8 < X < 256 are counted."
            // Add all tiles in this row to our tile list (left to right)
            for i in 0..sprite_w_tiles as i16 {
                if visible_tiles.len() < 34 {
                    visible_tiles.push(SpriteTile {
                        chr_addr: y_row_start_addr + 32 * i as u16,
                        x: sprite.x + 8 * i,
                        y_off: tile_y_off,
                        priority: sprite.priority,
                        palette: sprite.palette,
                    });
                } else {
                    // FIXME Set sprite tile overflow flag
                    break 'collect_tiles
                }
            }
        }

        self.render_state.visible_sprite_tiles = visible_tiles;
    }

    fn maybe_draw_sprite_pixel(&self, prio: u8) -> Option<Rgb> {
        if self.tm & 0x10 == 0 { return None }  // OBJ layer disabled

        for tile in &self.render_state.visible_sprite_tiles {
            if tile.priority == prio {
                // The tile must be on this scanline, we just have to check X
                if tile.x <= self.x as i16 && tile.x + 8 > self.x as i16 {
                    let x_offset = self.x as i16 - tile.x;
                    debug_assert!(0 <= x_offset && x_offset <= 7, "x_offset = {}", x_offset);
                    trace_unique!("rendering tile with CHR data at ${:04X}, palette {}",
                        tile.chr_addr, tile.palette);
                    let rel_color = self.read_chr_entry(4,  // 16 colors
                                                        tile.chr_addr,
                                                        8,  // 8x8 tiles
                                                        (x_offset as u8, tile.y_off));
                    debug_assert!(rel_color < 16, "rel_color = {} (but is 4-bit!)", rel_color);

                    let abs_color = 128 + tile.palette * 16 + rel_color;
                    // FIXME Color math
                    let rgb = self.lookup_color(abs_color);
                    return Some(rgb)
                }
            }
        }

        None
    }

    /// Determines if the given sprite is on the current scanline
    fn sprite_on_scanline(&self, sprite: &OamEntry) -> bool {
        let (w, h) = self.obj_size(sprite.size_toggle);
        let (w, h) = (w as i16, h);

        // "If any OBJ is at X=256 (or X=-256, same difference), consider it as being at X=0 when
        // considering Range and Time."
        // X=256 can not occur, since X is a signed 9-bit value (range is -256 - 255)
        let x = if sprite.x == -256 { 0 } else { sprite.x };

        // "Only those sprites with -size < X < 256 are considered in Range." (`size` is `w` here)
        // We don't check `X < 256`, since that cannot occur (X is a signed 9-bit integer)
        // A sprite moved past the right edge of the screen will wrap to `-256`, which is handled
        // by this check.
        if -w < x {
            // Sprites Y coordinate must be on the current scanline:
            sprite.y as u16 <= self.scanline && sprite.y as u16 + h as u16 >= self.scanline
        } else {
            false
        }
    }

    /// Applies color math to the given RGB value (if enabled), assuming it is the color of the
    /// current pixel.
    fn maybe_apply_color_math(&self, color: Rgb) -> Rgb {
        // FIXME needs more info (bg, no bg, ...)
        // TODO
        color
    }

    /// Lookup the color of the given background layer (1-4) at the current pixel, using the given
    /// priority (0-1) only. This will also scroll backgrounds accordingly and apply color math.
    ///
    /// Returns `None` if the pixel is transparent, `Some(Rgb)` otherwise.
    fn lookup_bg_color(&self, bg_num: u8, prio: u8) -> Option<Rgb> {
        debug_assert!(bg_num >= 1 && bg_num <= 4);
        if !self.bg_enabled(bg_num) { return None }

        // Apply BG scrolling and get the tile coordinates
        // FIXME Apply mosaic filter
        // FIXME Fix this: "Note that many games will set their vertical scroll values to -1 rather
        // than 0. This is because the SNES loads OBJ data for each scanline during the previous
        // scanline. The very first line, though, wouldn’t have any OBJ data loaded! So the SNES
        // doesn’t actually output scanline 0, although it does everything to render it. These
        // games want the first line of their tilemap to be the first line output, so they set
        // their VOFS registers in this manner. Note that an interlace screen needs -2 rather than
        // -1 to properly correct for the missing line 0 (and an emulator would need to add 2
        // instead of 1 to account for this)."
        let x = self.x;
        let y = self.scanline;
        let bg = self.bg_settings(bg_num);
        let tile_size = bg.tile_size;
        let (xscroll, yscroll) = (bg.hscroll, bg.vscroll);
        let tile_x = (x + xscroll) / tile_size as u16;
        let tile_y = (y + yscroll) / tile_size as u16;
        let off_x = ((x + xscroll) % tile_size as u16) as u8;
        let off_y = ((y + yscroll) % tile_size as u16) as u8;
        let (sx, sy) = (bg.tilemap_mirror_h, bg.tilemap_mirror_v);

        // Calculate the VRAM word address, where the tilemap entry for our tile is stored
        // FIXME Copied from http://wiki.superfamicom.org/snes/show/Backgrounds, no idea if correct
        // (or even correctly interpreted, since the wiki is really confusing)
        let tilemap_word_address =
            (bg.tilemap_addr << 9) +
            ((tile_y & 0x1f) << 5) +
            (tile_x & 0x1f) +
            if sy {(tile_y & 0x20) << if sx {6} else {5}} else {0} +
            if sx {(tile_x & 0x20) << 5} else {0};
        let tilemap_entry = self.tilemap_entry(tilemap_word_address);
        if tilemap_entry.priority != prio { return None }

        // Calculate the number of bitplanes needed to store a color in this BG
        let color_count = self.color_count_for_bg(bg_num);
        let bitplane_count = color_count.leading_zeros() as u16;
        debug_assert_eq!(color_count.count_ones(), 1);  // should be power of two

        // FIXME: Formula taken from the wiki, is this correct? In particular: `chr_base<<1`?
        let bitplane_start_addr =
            (bg.chr_addr << 1) +
            (tilemap_entry.tile_number * 8 * bitplane_count);

        let palette_index = self.read_chr_entry(bitplane_count as u8,
                                                bitplane_start_addr,
                                                tile_size,
                                                (off_x, off_y));

        match palette_index {
            0 => None,
            _ => Some(self.lookup_color(palette_index)),
        }
    }

    /// Reads character data for a pixel and returns the palette index stored in the bitplanes.
    ///
    /// # Parameters
    /// * `bitplane_count`: Number of bitplanes (must be even)
    /// * `start_addr`: Address of the first bitplane (or the first 2)
    /// * `tile_size`: 8 or 16
    /// * `(x, y)`: Offset inside the tile
    fn read_chr_entry(&self,
                      bitplane_count: u8,
                      start_addr: u16,
                      tile_size: u8,
                      (x, y): (u8, u8)) -> u8 {
        // 2 bitplanes are stored interleaved with each other.
        debug_assert!(bitplane_count & 1 == 0, "odd bitplane count");
        debug_assert!(tile_size == 8, "non-8x8 tiles unsupported"); // FIXME support 16x16 tiles
        let bitplane_pairs = bitplane_count >> 1;
        let bitplane_pair_size = 16;    // FIXME depends on tile size (?)

        // FIXME: I'm assuming all pairs of bitplanes are stored sequentially?
        let mut palette_index = 0u8;
        for i in (0..bitplane_pairs) {
            let bitplane_bits = self.read_2_bitplanes(
                start_addr + i as u16 * bitplane_pair_size,
                (x, y));
            palette_index = palette_index | (bitplane_bits << (2 * i));
        }

        palette_index
    }

    /// Reads 2 bits of the given coordinate within the bitplane's tile from 2 interleaved
    /// bitplanes.
    fn read_2_bitplanes(&self, bitplanes_start: u16, (x_off, y_off): (u8, u8)) -> u8 {
        // FIXME Handle flipped tiles somewhere in here (or not in here)
        // Bit 0 in low bytes, bit 1 in high bytes
        let lo = self.vram[bitplanes_start + y_off as u16 * 2];
        let hi = self.vram[bitplanes_start + y_off as u16 * 2 + 1];
        // X values in a byte: 01234567
        let bit0 = (lo >> (7 - x_off)) & 1;
        let bit1 = (hi >> (7 - x_off)) & 1;

        (bit1 << 1) | bit0
    }
}