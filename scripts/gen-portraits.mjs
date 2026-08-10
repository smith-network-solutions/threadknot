#!/usr/bin/env node
// Generates the Retro-Tech default portrait set: one original 24x24 pixel
// portrait per model family, emitted as crisp-edge SVGs into
// src/assets/portraits/. Each is an abstract robot bust drawn from scratch
// for this app (geometric heads, shading ramps, a spotlight backdrop):
// original work, deliberately unlike any existing game or brand character.
//
// Run: node scripts/gen-portraits.mjs
//
// Format: the generator paints the backdrop (base fill, a lighter spotlight
// panel behind the head, then any speckles), and the character map draws on
// top; "." is transparent so the backdrop shows through. Rows may be shorter
// than 24 chars; they are right-padded with "." automatically. Edit a map,
// re-run, done.

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "assets", "portraits");
const SIZE = 24;
const K = "#06070d"; // shared outline ink, same as the skin's --rt-frame

const SPRITES = {
  // The storyteller: a tall hooded figure, starlit. Purple ramp.
  fable: {
    bg: { base: "#171034", glow: "#241653", speckles: [[3, 3, "#ffd84a"], [20, 5, "#9a74ff"], [2, 15, "#6b3fd6"], [21, 17, "#ffd84a"]] },
    palette: {
      k: K, d: "#4a2a9e", h: "#6b3fd6", H: "#9a74ff",
      f: "#f0eaff", F: "#cfc2f0", e: "#181030", w: "#ffffff", s: "#ffd84a",
    },
    map: [
      "",
      "",
      "...........kk",
      "..........khHk",
      "..........khHhk",
      ".........khHhhk",
      ".........khHhhhk",
      "........khHhhhhhk",
      ".......khHhhhhhhhk",
      "......khHhhhhhhhhhk",
      ".....khHhhhhhhhhhhhk",
      "....khhhhhhhhhhhhhhhk",
      "....kdddddddddddddddk",
      "...kdkfffffffffffffkdk",
      "...kdkfweeffffeewfkdk",
      "...kdkfeeeffffeeefkdk",
      "....kkffffffffffffkk",
      ".....kfFFeeeeeeFFfk",
      ".....kffFFFFFFFFffk",
      "......kkffffffffkk",
      ".....kdhhhhhhhhhhdk",
      "....kdhhhhhhhhhhhhdk",
      "....kddddddddddddddk",
      "",
    ],
  },
  // The heavyweight: a broad crowned head on a deep red field. Gold ramp.
  opus: {
    bg: { base: "#380f15", glow: "#59161e", speckles: [[3, 4, "#ffd84a"], [20, 3, "#e0a41c"]] },
    palette: {
      k: K, D: "#8a5600", g: "#e0a41c", G: "#ffd84a",
      f: "#ffe9b8", F: "#eec27c", e: "#241505", r: "#ff4f6b", w: "#ffffff",
    },
    map: [
      "",
      "....kk.....kk.....kk",
      "....kGk...kGrk...kGk",
      "....kGGk..kGGk..kGGk",
      "....kGgGkkGgGkkGgGk",
      "....kGggggggggggggk",
      "....kggggggggggggDk",
      "....kDDDDDDDDDDDDDk",
      "...kkkkkkkkkkkkkkkkk",
      "...kffffffffffffffFk",
      "...kfweeffffffweefFk",
      "...kfeeeffffffeeefFk",
      "...kffffffffffffffFk",
      "...kfFFffffffffFFfFk",
      "...kffFeeeeeeeeFffFk",
      "...kffFFFFFFFFFFffFk",
      "...kfffffffffffffFFk",
      "....kkffffffffffkk",
      "...kDggggggggggggDk",
      "..kDgggggggggggggDDk",
      "..kDDDDDDDDDDDDDDDDk",
      "",
    ],
  },
  // The versewright: a sleek plumed helm with a glowing visor. Teal ramp.
  sonnet: {
    bg: { base: "#082421", glow: "#0e3a34", speckles: [[4, 3, "#7dfce4"], [20, 15, "#2bbfa4"]] },
    palette: {
      k: K, d: "#157a68", t: "#2bbfa4", T: "#7dfce4",
      v: "#071d1a", E: "#7dfce4", f: "#d9fff6", F: "#a8e3d4", w: "#ffffff",
    },
    map: [
      "",
      "......kTk",
      ".....kTTtk",
      "....kTTtk",
      "....kTtk",
      ".....kTtkkkkkkk",
      "....kkttttttttkk",
      "...kttttttttttttk",
      "..kttTTttttttttttk",
      "..kttttttttttttttk",
      "..kkkkkkkkkkkkkkkk",
      "..kvvvvvvvvvvvvvvk",
      "..kvEEwvvvvvvEEwvk",
      "..kvEEvvvvvvvEEvvk",
      "..kkkkkkkkkkkkkkkk",
      "...kffffffffffffk",
      "...kfFFeeeeeeFFfk".replace(/e/g, "v"),
      "....kffffffffffk",
      ".....kkkkkkkkkk",
      "....kdttttttttdk",
      "...kdttttttttttdk",
      "...kddddddddddddk",
      "",
    ],
  },
  // The featherweight: a small sprout-headed bot, low in the frame. Green ramp.
  haiku: {
    bg: { base: "#10260f", glow: "#1a3d18", speckles: [[4, 4, "#a5f2b0"], [19, 6, "#57d977"]] },
    palette: {
      k: K, d: "#2a7a41", g: "#57d977", G: "#a5f2b0",
      f: "#eafff0", F: "#c2ebd0", e: "#12331c", w: "#ffffff",
    },
    map: [
      "",
      "",
      "",
      "......kGk..kGk",
      ".......kGkkGk",
      "........kggk",
      "........kgk",
      "........kgk",
      "......kkkgkkk",
      "....kkgggggggkk",
      "...kgggggggggggk",
      "..kggGGGGGGGGGggk",
      "..kgGfffffffffGgk",
      "..kgGfweeffeewfGgk",
      "..kgGfeeeffeeefGgk",
      "..kgGfffffffffGgk",
      "..kgGfFeeeeeeFfGgk",
      "...kgGffffffffGgk",
      "....kkGGGGGGGGkk",
      ".....kdddddddk",
      "....kdddddddddk",
      ".....kkkkkkkkk",
      "",
    ],
  },
  // The steady hand: a warm round helm with one antenna. Coral ramp (agent default).
  claude: {
    bg: { base: "#33150c", glow: "#4d2113", speckles: [[3, 5, "#ffd84a"], [20, 4, "#ffb08a"]] },
    palette: {
      k: K, d: "#b85a33", o: "#ff8a5c", O: "#ffb08a",
      f: "#ffe8dc", F: "#f2c4ad", e: "#33150c", w: "#ffffff", a: "#ffd84a",
    },
    map: [
      "",
      "...........ka",
      "...........kk",
      "........kkkokkkk",
      "......kkooooooookk",
      ".....koooOOOOOoooook".slice(0, 19) + "k",
      "....koOOOOOOOOOOOook",
      "....koOkkkkkkkkkkOok",
      "....koOkffffffffkOok",
      "....koOkfweffwefkOok",
      "....koOkfeeffeefkOok",
      "....koOkffffffffkOok",
      "....koOkfFeeeeFfkOok",
      "....koOkffFFFFffkOok",
      "....koOkffffffffkOok",
      ".....koOkkkkkkkkOok",
      "......koooooooooook".slice(0, 18) + "k",
      ".......kkddddddkk",
      "......kdooooooookk".slice(0, 17) + "k",
      ".....kdooooooooooodk".slice(0, 19) + "k",
      ".....kdddddddddddddk".slice(0, 19) + "k",
      "",
    ],
  },
  // The console: a CRT head with a live prompt, on a stand. Cyan ramp.
  codex: {
    bg: { base: "#081326", glow: "#0e2140", speckles: [[4, 18, "#7de5ff"], [20, 4, "#22b8e8"]] },
    palette: {
      k: K, d: "#0f6e94", c: "#22b8e8", C: "#7de5ff",
      s: "#04121e", p: "#7dffb0", P: "#2bd977", w: "#ffffff",
    },
    map: [
      "",
      "",
      "...kkkkkkkkkkkkkkkk",
      "..kccCCCCCCCCCCCCcck".slice(0, 19) + "k",
      "..kcCkkkkkkkkkkkkCck".slice(0, 19) + "k",
      "..kcCksssssssssskCck".slice(0, 19) + "k",
      "..kcCkspwssssssskCck".slice(0, 19) + "k",
      "..kcCksppPsssssskCck".slice(0, 19) + "k",
      "..kcCkspwssppssskCck".slice(0, 19) + "k",
      "..kcCkssssssssssKCck".replace("K", "k").slice(0, 19) + "k",
      "..kcCksPPPPPPssskCck".slice(0, 19) + "k",
      "..kcCksssssssssskCck".slice(0, 19) + "k",
      "..kcCkkkkkkkkkkkkCck".slice(0, 19) + "k",
      "..kccccccccccccccdck".slice(0, 19) + "k",
      "...kkkkkkkkkkkkkkkk",
      "........kddk",
      "........kddk",
      "......kkddddkk",
      ".....kcccccccck",
      ".....kdddddddddk".slice(0, 15) + "k",
      "",
      "",
      "",
    ],
  },
  // The comet: a finned helm with a star crest and a trailing scarf. Magenta ramp.
  kimi: {
    bg: { base: "#2b0c22", glow: "#451436", speckles: [[3, 4, "#ffd84a"], [20, 8, "#ff9ccb"], [4, 18, "#ff4fa3"]] },
    palette: {
      k: K, d: "#b03072", m: "#ff4fa3", M: "#ff9ccb",
      f: "#ffe4f2", F: "#f2bcd8", e: "#33101f", w: "#ffffff", s: "#ffd84a",
    },
    map: [
      "",
      "...........ks",
      "..........ksss",
      "...........ks",
      "........kkkmkkk",
      "......kkmmmmmmmkk",
      ".....kmmMMMMMMMmmk",
      "....kmMMMMMMMMMMMmk",
      "....kmMMkkkkkkkMMmk",
      "....kmMkfffffffkMmk",
      "....kmMkfwefwefkMmk",
      "....kmMkfeefeefkMmk",
      "....kmMkfffffffkMmk",
      "....kmMkfFeeeFfkMmk",
      "....kmMkffFFFffkMmk",
      ".....kmMkkkkkkkMmk",
      "......kmmmmmmmmmk",
      ".....kdmmk...kmmdk",
      "......kkk.....kkk",
      "",
      "",
    ],
  },
  // The courier: a winged helm over a calm face. Sky ramp.
  hermes: {
    bg: { base: "#0d1a33", glow: "#152a50", speckles: [[3, 6, "#8fb6ff"], [20, 5, "#dfe9ff"]] },
    palette: {
      k: K, d: "#3b62c4", b: "#5b86e8", W: "#8fb6ff",
      u: "#dfe9ff", f: "#f2f7ff", F: "#cfdcf2", e: "#101a30", w: "#ffffff",
    },
    map: [
      "",
      "",
      "",
      "......kkkkkkkkkk",
      ".....kWWWWWWWWWWk",
      "..kukWWWWWWWWWWWWkuk",
      ".kuukWWbbbbbbbbWWkuuk",
      "kuuukWbbbbbbbbbbWkuuuk",
      ".kukkWbbbbbbbbbbWkkuk",
      "...kkkkkkkkkkkkkkkk",
      "....kffffffffffffk",
      "....kfweeffffeewfk",
      "....kfeeeffffeeefk",
      "....kffffffffffffk",
      "....kfFFeeeeeeFFfk",
      "....kffFFFFFFFFffk",
      ".....kkffffffffkk",
      "......kddddddddk",
      ".....kdbbbbbbbbdk",
      "....kdbbbbbbbbbbdk",
      "....kddddddddddddk",
      "",
      "",
    ],
  },
};

mkdirSync(OUT, { recursive: true });
for (const [name, { bg, palette, map }] of Object.entries(SPRITES)) {
  if (map.length > SIZE) throw new Error(`${name}: ${map.length} rows (max ${SIZE})`);
  const rects = [];
  // Backdrop: base fill, spotlight panel behind the head, speckles.
  rects.push(`<rect x="0" y="0" width="${SIZE}" height="${SIZE}" fill="${bg.base}"/>`);
  rects.push(`<rect x="4" y="3" width="16" height="18" fill="${bg.glow}"/>`);
  for (const [x, y, color] of bg.speckles ?? []) {
    rects.push(`<rect x="${x}" y="${y}" width="1" height="1" fill="${color}"/>`);
  }
  map.forEach((row, y) => {
    if (row.length > SIZE) throw new Error(`${name}: row ${y} is ${row.length} chars (max ${SIZE})`);
    for (let x = 0; x < row.length; x++) {
      const ch = row[x];
      if (ch === ".") continue;
      const fill = palette[ch];
      if (!fill) throw new Error(`${name}: no palette entry for "${ch}" at ${x},${y}`);
      rects.push(`<rect x="${x}" y="${y}" width="1" height="1" fill="${fill}"/>`);
    }
  });
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${SIZE} ${SIZE}" width="96" height="96" shape-rendering="crispEdges">${rects.join("")}</svg>\n`;
  writeFileSync(join(OUT, `${name}.svg`), svg);
  console.log(`wrote ${name}.svg (${rects.length} rects)`);
}
