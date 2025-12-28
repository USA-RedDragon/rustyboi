// Gambatte-AGB bootstrap reference generator.
//
// Runs each ROM through libgambatte in AGB mode (CGB_MODE | GBA_FLAG, the same
// flags Gambatte's own testrunner uses for its 'a' / agbout path) and prints
// one line per ROM:
//
//     <fnv1a-64-hex-of-final-framebuffer>\t<rom-path>
//
// The framebuffer is masked with 0xF8F8F8 before hashing, matching Gambatte's
// own frame comparison (frameBufsEqual / tilesAreEqual drop the low 3 bits of
// each channel). The post-boot run length (NO_BIOS, 15 frames) mirrors the
// testrunner's no-bios path so rustyboi's default 15-frame run lines up.
//
// This is the AGB validation ORACLE: rustyboi's Hardware::AGB framebuffer hash
// is compared against these hashes per ROM. Built against the repo's prebuilt
// libgambatte.a (read-only). Build with tools/build_agb_ref.sh.

#include "gambatte.h"
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>

namespace {
unsigned const gb_width = 160, gb_height = 144;
std::size_t const samples_per_frame = 35112;
std::size_t const audiobuf_size = samples_per_frame + 2064;
std::size_t const framebuf_size = gb_width * gb_height;

std::uint64_t fnv1a(gambatte::uint_least32_t const *buf, std::size_t n) {
    std::uint64_t h = 1469598103934665603ull;
    for (std::size_t i = 0; i < n; ++i) {
        std::uint32_t v = static_cast<std::uint32_t>(buf[i]) & 0xF8F8F8u;
        for (int b = 0; b < 4; ++b) {
            h ^= (v >> (b * 8)) & 0xFF;
            h *= 1099511628211ull;
        }
    }
    return h;
}

// Run a ROM in AGB mode, no-bios (post-boot state), 15 frames; hash the frame.
bool runAgb(std::string const &romfile, std::uint64_t &outHash) {
    gambatte::GB gb;
    int const flags = gambatte::GB::LoadFlag::CGB_MODE
                    | gambatte::GB::LoadFlag::GBA_FLAG
                    | gambatte::GB::LoadFlag::NO_BIOS;
    if (gb.load(romfile, flags))
        return false;

    // Same CGB palette LUT as the testrunner's cgb path.
    unsigned lut[32768];
    int i = 0;
    for (int b = 0; b < 32; b++)
        for (int g = 0; g < 32; g++)
            for (int r = 0; r < 32; r++)
                lut[i++] = ((r * 3 + g * 2 + b * 11) >> 1)
                         | ((g * 3 + b) << 1) << 8
                         | ((r * 13 + g * 2 + b) >> 1) << 16
                         | 255 << 24;
    gb.setCgbPalette(lut);

    static gambatte::uint_least32_t framebuf[framebuf_size];
    static gambatte::uint_least32_t audiobuf[audiobuf_size];
    long samplesLeft = static_cast<long>(samples_per_frame) * 15; // no-bios: 15 frames
    while (samplesLeft >= 0) {
        std::size_t samples = samples_per_frame;
        gb.runFor(framebuf, gb_width, audiobuf, samples);
        samplesLeft -= static_cast<long>(samples);
    }
    outHash = fnv1a(framebuf, framebuf_size);
    return true;
}
} // namespace

int main(int argc, char *argv[]) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: %s <rom> [rom...]\n", argv[0]);
        return 2;
    }
    for (int a = 1; a < argc; ++a) {
        std::uint64_t hash = 0;
        if (runAgb(argv[a], hash)) {
            std::printf("%016llx\t%s\n",
                        static_cast<unsigned long long>(hash), argv[a]);
        } else {
            std::printf("LOADFAIL\t%s\n", argv[a]);
        }
    }
    return 0;
}
