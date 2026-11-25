import json, struct, sys

S = [0x7F800000, 0xFF800000, 0x7FC00000, 0xFFC00000, 0x7F800001, 0x7FC12345, 0x7FBFFFFF, 0xFF800001,
     0x477FE000, 0x477FEFFF, 0x477FF000, 0x477FFF00, 0x47C35000, 0x4788B800, 0xC7C35000, 0x7F7FFFFF, 0xFF7FFFFF,
     0x3F800000, 0xBF800000, 0x00000000, 0x80000000, 0x322BCC77, 0x33800000, 0x33000000, 0x477E0000, 0x477E4C00,
     0x4780E800, 0x477C0000, 0x3FC00000, 0x38000000, 0x38800000, 0x471C4000, 0x00000001, 0x807FFFFF]
D = [0x00000000, 0x3F800000, 0x471C4000, 0x477FE000, 0x477E0000, 0x7F800000, 0xBF800000, 0xFF800000, 0x7FC00000, 0xFFC00000]
PAIRS = [(s, d) for d in D for s in S]
W = len(PAIRS)
FORMATS = [("rgba16f", 8), ("r11g11b10", 4), ("rgba32f", 16), ("r16f", 2)]
MODES = ["off", "one_zero", "add", "min", "max"]


def fl(u):
    return struct.unpack("<f", struct.pack("<I", u))[0]


def gen(path):
    with open(path, "wb") as f:
        for s, d in PAIRS:
            f.write(struct.pack("<4I", s, s, s, s))
        for s, d in PAIRS:
            f.write(struct.pack("<4I", d, d, d, d))
    print(W)


def load(path):
    b = open(path, "rb").read()
    out, o = {}, 0
    for name, bpp in FORMATS:
        for m in MODES:
            out[(name, m)] = (b[o:o + W * bpp], bpp)
            o += W * bpp
    return out


def px(r, f, m, i):
    b, bpp = r[(f, m)]
    if f in ("rgba16f", "r16f"):
        return struct.unpack_from("<H", b, bpp * i)[0]
    if f == "rgba32f":
        return struct.unpack_from("<I", b, bpp * i)[0]
    w = struct.unpack_from("<I", b, 4 * i)[0]
    return (w & 0x7FF, (w >> 11) & 0x7FF, w >> 22)


def nan(f, v):
    if f in ("rgba16f", "r16f"):
        return v & 0x7C00 == 0x7C00 and v & 0x3FF != 0
    if f == "rgba32f":
        return v & 0x7F800000 == 0x7F800000 and v & 0x7FFFFF != 0
    return v[0] & 0x7C0 == 0x7C0 and v[0] & 0x3F != 0


def cls(u):
    e, m = (u >> 23) & 0xFF, u & 0x7FFFFF
    if e == 0xFF:
        return "nan" if m else ("+inf" if u >> 31 == 0 else "-inf")
    return "num"


def show(path):
    r = load(path)
    for f, _ in FORMATS:
        for m in MODES:
            print("==", f, m)
            for i, (s, d) in enumerate(PAIRS):
                if m in ("off", "one_zero") and d != 0:
                    continue
                v = px(r, f, m, i)
                vs = "%04x" % v if isinstance(v, int) and f != "rgba32f" else "%08x" % v if isinstance(v, int) else "%03x %03x %03x" % v
                print("  src %08x (%-12g) dst %08x (%-8g) -> %s" % (s, fl(s), d, fl(d), vs))


def check(path):
    r = load(path)
    bad = 0
    for f in ("rgba16f", "r16f", "r11g11b10"):
        inf = 0x7C00 if f != "r11g11b10" else (0x7C0, 0x7C0, 0x3E0)
        mx = 0x7BFF if f != "r11g11b10" else (0x7BF, 0x7BF, 0x3DF)
        for m in ("off", "one_zero", "add"):
            for i, (s, d) in enumerate(PAIRS):
                v = px(r, f, m, i)
                cs, cd = cls(s), cls(d)
                if f == "r11g11b10" and cd in ("-inf",):
                    cd = "num"
                if m == "add" and cd == "nan":
                    want = "nan"
                elif cs == "nan":
                    want = "nan"
                elif m == "add" and cs == "+inf" and cd == "-inf" and f != "r11g11b10":
                    want = "nan"
                elif m == "add" and cs == "-inf" and cd == "+inf":
                    want = "nan"
                elif cs == "+inf":
                    want = "inf"
                elif m == "add" and cd == "+inf":
                    want = "inf"
                else:
                    continue
                ok = nan(f, v) if want == "nan" else v == inf
                if not ok:
                    bad += 1
                    print("differs:", f, m, "%08x %08x" % (s, d), v)
        for m in ("min", "max"):
            for i, (s, d) in enumerate(PAIRS):
                if cls(s) == "nan" and cls(d) != "nan":
                    v, o = px(r, f, m, i), px(r, f, "off", S.index(d))
                    if v != o:
                        bad += 1
                        print("differs:", f, m, "%08x %08x" % (s, d), v, o)
        for i, (s, d) in enumerate(PAIRS):
            if d == 0x471C4000 and s == 0x471C4000 and px(r, f, "add", i) != mx:
                bad += 1
                print("differs:", f, "add 40000 + 40000", px(r, f, "add", i))
    print("%d results differ from the gfx1036 rules" % bad)
    return bad


def main():
    mode = sys.argv[1]
    if mode == "gen":
        gen(sys.argv[2])
    elif mode == "show":
        show(sys.argv[2])
    else:
        sys.exit(1 if check(sys.argv[2]) else 0)


main()
