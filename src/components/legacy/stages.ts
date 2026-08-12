// The three games behind the hidden circuit.
//
// Everything here is original: the mechanics, the sprites, the mazes and the
// names. The debt this pays is to a *way of building things* (a fixed low
// resolution, a handful of colours, one screen at a time), not to any
// particular cabinet, and nothing in this file reproduces artwork, audio,
// characters or branding from one.
//
// The stages know nothing about React, sound or the CRT frame around them.
// Each is a factory returning a run with the same four moving parts: take
// input, advance by a timestep, draw into a 320x240 buffer, and raise cues the
// shell turns into noise. That is the whole contract, which is why a fourth
// game would be a new entry in STAGES and nothing else.
//
// Drawing goes through raster.ts. Nothing in here paints a sprite pixel by
// pixel at frame time: sprites and static backdrops are baked once and blitted.

import { drawSprite, StaticLayer, type Palette, type PixelMap } from "./raster";

/** Logical resolution. The canvas is this size and CSS scales it by a whole
 *  number, so a pixel is a pixel and nothing is ever half-lit. */
export const VIEW_W = 320;
export const VIEW_H = 240;

const CELL = 16;
const COLS = VIEW_W / CELL; // 20
const ROWS = VIEW_H / CELL; // 15

/** The cabinet palette. Deliberately small, and shared by all three games so
 *  the campaign reads as one machine rather than three. */
export const IN = {
  ink: "#05040f",
  grid: "#141046",
  wire: "#2de1c2",
  wireDim: "#12705f",
  hot: "#ff3d8a",
  gold: "#ffcf4d",
  pale: "#bdf3ff",
  dusk: "#5b4fb0",
  violet: "#a06bff",
  rust: "#ff8a3d",
} as const;

export type Btn = "up" | "down" | "left" | "right" | "fire" | "bomb";

export interface Input {
  /** Buttons currently down. */
  held: ReadonlySet<Btn>;
  /** Buttons that went down since the previous tick. */
  tapped: ReadonlySet<Btn>;
}

export type Cue =
  | "step"
  | "pickup"
  | "shoot"
  | "hit"
  | "clear"
  | "die"
  | "gate"
  | "power"
  | "boom"
  | "clank";

export type RunStatus = "playing" | "cleared" | "failed";

export interface StageRun {
  status: RunStatus;
  /** Points banked in this level so far. Added to the campaign total when the
   *  level is cleared, and thrown away when a life is lost, so a life is worth
   *  something beyond the counter. */
  score: number;
  /** The one line of level-specific state the HUD shows. */
  readout(): string;
  update(dt: number, input: Input): void;
  draw(g: CanvasRenderingContext2D): void;
  /** Hand over (and clear) the sound cues raised since the last call. */
  drain(): Cue[];
}

/** Every game is three levels. Nine in a campaign. */
export const LEVELS_PER_STAGE = 3;

export interface Stage {
  id: string;
  title: string;
  brief: string;
  controls: string;
  levelNote(level: number): string;
  create(seed: number, level: number): StageRun;
}

function lvl(level: number): number {
  return Math.max(0, Math.min(LEVELS_PER_STAGE - 1, Math.floor(level)));
}

/* -------------------------------------------------------------------------- */
/* Small shared parts                                                          */
/* -------------------------------------------------------------------------- */

/** A seeded generator, so a level plays out the same way for the same seed.
 *  Worth the six lines: "it only happens sometimes" is not a bug report anyone
 *  can act on, and a seed makes it one that is. */
function makeRandom(seed: number): () => number {
  let s = (seed >>> 0) || 0x9e3779b9;
  return () => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 0x100000000;
  };
}

function aabb(
  ax: number, ay: number, aw: number, ah: number,
  bx: number, by: number, bw: number, bh: number,
): boolean {
  return ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by;
}

interface Cell {
  x: number;
  y: number;
}

/** The faint back-of-the-tube grid. Painted once per run, not per frame. */
function fieldLayer(): StaticLayer {
  return new StaticLayer(VIEW_W, VIEW_H, (g) => {
    g.fillStyle = IN.ink;
    g.fillRect(0, 0, VIEW_W, VIEW_H);
    g.fillStyle = IN.grid;
    for (let x = CELL; x < VIEW_W; x += CELL) g.fillRect(x, 0, 1, VIEW_H);
    for (let y = CELL; y < VIEW_H; y += CELL) g.fillRect(0, y, VIEW_W, 1);
  });
}

/* -------------------------------------------------------------------------- */
/* Game 1: KNOTLINE                                                            */
/* -------------------------------------------------------------------------- */

const KNOT_LEVELS = [
  { target: 8, step: 0.14, quicken: 0.007, severed: 4, floor: 0.075, fault: 0 },
  { target: 10, step: 0.122, quicken: 0.0078, severed: 6, floor: 0.064, fault: 0 },
  // The third level adds the fault: a break in the board that walks toward the
  // head, one step for every two of yours. It cannot be outrun on a board this
  // size, so the level stops being about the nodes and becomes about where you
  // are going to be in six moves.
  { target: 12, step: 0.108, quicken: 0.008, severed: 8, floor: 0.055, fault: 2 },
] as const;

/**
 * A thread you are paying out across a board. It grows every time it reaches a
 * code node, and it is the only thing on the board that can kill you: the walls
 * of the frame, the severed circuits that open up as you go, and the length of
 * thread already behind you.
 */
function createKnotline(seed: number, level: number): StageRun {
  const L = KNOT_LEVELS[lvl(level)];
  const TARGET = L.target;
  const rand = makeRandom(seed);
  const bg = fieldLayer();
  // The frame is static too: bake it into the backdrop rather than redrawing
  // eighty little rivets every frame.
  const frame = new StaticLayer(VIEW_W, VIEW_H, (g) => {
    g.fillStyle = IN.dusk;
    g.fillRect(0, 0, VIEW_W, CELL);
    g.fillRect(0, VIEW_H - CELL, VIEW_W, CELL);
    g.fillRect(0, 0, CELL, VIEW_H);
    g.fillRect(VIEW_W - CELL, 0, CELL, VIEW_H);
    g.fillStyle = IN.ink;
    for (let x = 0; x < VIEW_W; x += 4) {
      g.fillRect(x + 1, 5, 2, 6);
      g.fillRect(x + 1, VIEW_H - 11, 2, 6);
    }
    for (let y = 0; y < VIEW_H; y += 4) {
      g.fillRect(5, y + 1, 6, 2);
      g.fillRect(VIEW_W - 11, y + 1, 6, 2);
    }
  });

  let body: Cell[] = [{ x: 6, y: 7 }, { x: 5, y: 7 }, { x: 4, y: 7 }];
  let dir: Cell = { x: 1, y: 0 };
  const queued: Cell[] = [];
  let grow = 0;
  let taken = 0;
  let acc = 0;
  let stepMs: number = L.step;
  let node: Cell = { x: 13, y: 7 };
  const severed: Cell[] = [];
  const cues: Cue[] = [];
  let status: RunStatus = "playing";
  let score = 0;
  let flash = 0;
  /** The walking break, on levels that have one. Starts in the far corner. */
  let fault: Cell | null = L.fault > 0 ? { x: COLS - 3, y: ROWS - 3 } : null;
  let faultTick = 0;

  const occupied = (c: Cell): boolean =>
    body.some((b) => b.x === c.x && b.y === c.y) ||
    severed.some((s) => s.x === c.x && s.y === c.y) ||
    (fault !== null && fault.x === c.x && fault.y === c.y) ||
    (node.x === c.x && node.y === c.y);

  function freeCell(): Cell {
    for (let i = 0; i < 200; i++) {
      const c = { x: 1 + Math.floor(rand() * (COLS - 2)), y: 1 + Math.floor(rand() * (ROWS - 2)) };
      if (!occupied(c)) return c;
    }
    for (let y = 1; y < ROWS - 1; y++) {
      for (let x = 1; x < COLS - 1; x++) if (!occupied({ x, y })) return { x, y };
    }
    return { x: 1, y: 1 };
  }

  node = freeCell();

  function step(): void {
    const next = queued.shift();
    if (next && !(next.x === -dir.x && next.y === -dir.y)) dir = next;
    const head = { x: body[0].x + dir.x, y: body[0].y + dir.y };
    const hitWall = head.x <= 0 || head.y <= 0 || head.x >= COLS - 1 || head.y >= ROWS - 1;
    const hitSelf = body.some((b, i) => i < body.length - 1 && b.x === head.x && b.y === head.y);
    const hitSevered = severed.some((s) => s.x === head.x && s.y === head.y);
    const hitFault = fault !== null && fault.x === head.x && fault.y === head.y;
    if (hitWall || hitSelf || hitSevered || hitFault) {
      status = "failed";
      cues.push("die");
      return;
    }

    // The fault closes in every few steps, on one axis at a time, so it reads
    // as something walking rather than something teleporting.
    if (fault && L.fault > 0 && ++faultTick >= L.fault) {
      faultTick = 0;
      const dx = head.x - fault.x;
      const dy = head.y - fault.y;
      const stepX = Math.abs(dx) >= Math.abs(dy);
      const nx = fault.x + (stepX ? Math.sign(dx) : 0);
      const ny = fault.y + (stepX ? 0 : Math.sign(dy));
      if (nx > 0 && ny > 0 && nx < COLS - 1 && ny < ROWS - 1) fault = { x: nx, y: ny };
      if (fault.x === head.x && fault.y === head.y) {
        status = "failed";
        cues.push("die");
        return;
      }
    }
    body.unshift(head);
    if (grow > 0) grow--;
    else body.pop();

    if (head.x === node.x && head.y === node.y) {
      taken++;
      grow += 3;
      score += 25;
      flash = 0.18;
      stepMs = Math.max(L.floor, stepMs - L.quicken);
      cues.push("pickup");
      if (taken >= TARGET) {
        status = "cleared";
        score += 100;
        cues.push("clear");
        return;
      }
      if (taken >= 2 && severed.length < L.severed) severed.push(freeCell());
      node = freeCell();
    } else {
      cues.push("step");
    }
  }

  return {
    get status() { return status; },
    get score() { return score; },
    readout: () => `NODES ${taken}/${TARGET}`,
    update(dt, input) {
      if (status !== "playing") return;
      if (flash > 0) flash = Math.max(0, flash - dt);
      const push = (c: Cell) => { if (queued.length < 2) queued.push(c); };
      if (input.tapped.has("up")) push({ x: 0, y: -1 });
      if (input.tapped.has("down")) push({ x: 0, y: 1 });
      if (input.tapped.has("left")) push({ x: -1, y: 0 });
      if (input.tapped.has("right")) push({ x: 1, y: 0 });

      acc += dt;
      let guard = 8;
      while (acc >= stepMs && status === "playing" && guard-- > 0) {
        acc -= stepMs;
        step();
      }
      if (guard <= 0) acc = 0;
    },
    draw(g) {
      bg.draw(g);
      frame.draw(g);

      for (const s of severed) {
        const px = s.x * CELL;
        const py = s.y * CELL;
        g.fillStyle = IN.hot;
        for (let i = 2; i < CELL - 2; i++) {
          g.fillRect(px + i, py + i, 2, 2);
          g.fillRect(px + (CELL - 2 - i), py + i, 2, 2);
        }
      }

      // The fault: a filled diamond, so it never reads as a severed cell.
      if (fault) {
        g.fillStyle = IN.rust;
        for (let i = 0; i < 7; i++) {
          const w = 2 + i * 2 - (i > 3 ? (i - 3) * 4 : 0);
          g.fillRect(fault.x * CELL + 8 - w / 2, fault.y * CELL + 2 + i * 2, w, 2);
        }
        g.fillStyle = IN.gold;
        g.fillRect(fault.x * CELL + 7, fault.y * CELL + 7, 2, 2);
      }

      g.fillStyle = flash > 0 ? IN.pale : IN.gold;
      g.fillRect(node.x * CELL + 4, node.y * CELL + 4, 8, 8);
      g.fillRect(node.x * CELL + 6, node.y * CELL + 1, 4, 2);
      g.fillRect(node.x * CELL + 6, node.y * CELL + 13, 4, 2);
      g.fillRect(node.x * CELL + 1, node.y * CELL + 6, 2, 4);
      g.fillRect(node.x * CELL + 13, node.y * CELL + 6, 2, 4);

      // Runs of one colour are drawn without re-setting fillStyle per segment:
      // the tail is one pass, the mid another, the head last.
      const n = body.length;
      g.fillStyle = IN.wireDim;
      for (let i = n - 1; i >= 1; i--) {
        if (1 - i / Math.max(1, n) > 0.45) continue;
        g.fillRect(body[i].x * CELL + 3, body[i].y * CELL + 3, CELL - 6, CELL - 6);
      }
      g.fillStyle = IN.wire;
      for (let i = n - 1; i >= 1; i--) {
        if (1 - i / Math.max(1, n) <= 0.45) continue;
        g.fillRect(body[i].x * CELL + 3, body[i].y * CELL + 3, CELL - 6, CELL - 6);
      }
      g.fillStyle = IN.pale;
      g.fillRect(body[0].x * CELL + 2, body[0].y * CELL + 2, CELL - 4, CELL - 4);
    },
    drain() { return cues.splice(0, cues.length); },
  };
}

/* -------------------------------------------------------------------------- */
/* Game 2: BUG BLASTER                                                         */
/* -------------------------------------------------------------------------- */

/** Four kinds of mite, four silhouettes. Shape carries the meaning: colour
 *  alone would be useless to anyone who cannot separate teal from gold. */
const SPRITES: Record<string, [PixelMap, PixelMap]> = {
  // The plain one: two brackets and four legs.
  grub: [
    ["..#..#..", ".######.", "##.##.##", "########", "#.#..#.#"],
    ["#.#..#.#", ".######.", "##.##.##", "########", "..#..#.."],
  ],
  // Armoured: a solid carapace with a seam. Takes two hits.
  shell: [
    [".######.", "########", "##.##.##", "########", ".#.##.#."],
    [".######.", "########", "##.##.##", "########", "#..##..#"],
  ],
  // Pointed downward, because it is going to come at you.
  darter: [
    ["##....##", ".######.", "..####..", "...##...", "...##..."],
    ["#......#", "########", ".#####..", "..###...", "...##..."],
  ],
  // Wide, with a nozzle underneath.
  spitter: [
    ["..####..", ".######.", "########", "#.#..#.#", "...##..."],
    ["..####..", ".######.", "########", "#.#..#.#", "..#..#.."],
  ],
  // The hive. Twice the width of anything else and it makes more of them.
  hive: [
    ["..########..", ".##########.", "####....####", "##.######.##", "####....####", ".#.##..##.#.", "#..#....#..#"],
    ["..########..", ".##########.", "####....####", "##.######.##", "####....####", "#.#.####.#.#", ".#..#..#..#."],
  ],
};

type MiteKind = "grub" | "shell" | "darter" | "spitter" | "hive";

interface MiteSpec {
  hp: number;
  points: number;
  colour: string;
  /** How much more often than the base rate this kind opens fire. */
  fire: number;
  w: number;
  h: number;
}

const MITES: Record<MiteKind, MiteSpec> = {
  grub: { hp: 1, points: 15, colour: IN.wire, fire: 1, w: 8, h: 5 },
  shell: { hp: 2, points: 30, colour: IN.gold, fire: 0.7, w: 8, h: 5 },
  darter: { hp: 1, points: 40, colour: IN.hot, fire: 0.4, w: 8, h: 5 },
  spitter: { hp: 2, points: 50, colour: IN.violet, fire: 2.4, w: 8, h: 5 },
  hive: { hp: 10, points: 400, colour: IN.rust, fire: 1.6, w: 12, h: 7 },
};

const CANNON: PixelMap = ["...##...", "..####..", ".######.", "########"];

const MITE_W = 8;
const MITE_H = 5;
const PLAYER_Y = VIEW_H - 22;

/** Per level: the waves it puts up (as rows of kinds), the gun you start with,
 *  and how talkative the wall is. */
const BLAST_LEVELS = [
  {
    waves: [
      { cols: 6, rows: ["grub", "grub", "grub"], hive: false },
      { cols: 7, rows: ["shell", "grub", "grub"], hive: false },
    ],
    startTier: 0,
    // The gentlest wall in the campaign. You are on a two-shot peashooter here,
    // so the wall has to be correspondingly quiet or the first level of the
    // first game is harder than the second.
    fireBase: 2,
    divers: 0,
  },
  {
    waves: [
      { cols: 7, rows: ["shell", "grub", "darter"], hive: false },
      { cols: 8, rows: ["spitter", "shell", "grub"], hive: false },
    ],
    startTier: 1,
    fireBase: 1.35,
    divers: 1,
  },
  {
    waves: [
      { cols: 8, rows: ["spitter", "shell", "darter", "grub"], hive: false },
      // The last wave of the last level puts a hive over the wall. It takes ten
      // hits, it shoots, and it keeps making grubs until it is dead, so the
      // wall you are clearing refills while you clear it.
      { cols: 8, rows: ["shell", "spitter", "darter"], hive: true },
    ],
    startTier: 2,
    fireBase: 1.2,
    divers: 3,
  },
] as const;

const TIERS = [
  { name: "BOLT", maxShots: 2, cooldown: 0.2, speed: 340, pierce: 1, units: 1, width: 2 },
  { name: "RAPID", maxShots: 4, cooldown: 0.12, speed: 380, pierce: 1, units: 1, width: 2 },
  { name: "TWIN", maxShots: 6, cooldown: 0.12, speed: 380, pierce: 1, units: 2, width: 2 },
  { name: "MISSILE", maxShots: 8, cooldown: 0.09, speed: 460, pierce: 3, units: 2, width: 3 },
] as const;

const MAX_BOMBS = 3;
const POD_OFFSET = 18;

interface Mite {
  cx: number;
  cy: number;
  kind: MiteKind;
  hp: number;
  alive: boolean;
  /** Frames of white flash left after a hit that did not kill it. */
  hurt: number;
  /** Set when it has left the formation and is coming for you. */
  diving: boolean;
  dx: number;
  dy: number;
  /** Free-flight position once diving. */
  fx: number;
  fy: number;
}

interface Shot { x: number; y: number; live: boolean }
interface Bolt extends Shot { pierce: number }
interface Drop { x: number; y: number; live: boolean; kind: "power" | "bomb" }

/**
 * A wall of mites walking down the stack, in four varieties that want
 * different things from you. Grubs die to anything. Shells need two hits.
 * Spitters return fire far more often than the rest, so they are the ones to
 * kill first. Darters leave the formation and come straight at you.
 *
 * You start with a peashooter; clearing a wave promotes the gun and the mites
 * drop the rest, so by the last level a well-armed player has two units firing
 * piercing missiles and a bomb in reserve.
 */
function createBugBlaster(seed: number, level: number): StageRun {
  const L = BLAST_LEVELS[lvl(level)];
  const rand = makeRandom(seed);
  const bg = fieldLayer();

  let wave = 0;
  let mites: Mite[] = [];
  let originX = 0;
  let originY = 0;
  let marchDir = 1;
  let animAcc = 0;
  let animFrame = 0;
  let fireTimer = 0;
  let diveTimer = 4;
  let divesLeft = 0;
  let hiveTimer = 5;
  let playerX = VIEW_W / 2 - 4;
  let tier: number = L.startTier;
  let bombs = 1;
  let cooldown = 0;
  let bombFlash = 0;
  const bolts: Bolt[] = [];
  const drops: Drop[] = [];
  const glitches: Shot[] = [];
  let status: RunStatus = "playing";
  let score = 0;
  const cues: Cue[] = [];
  let interlude = 0;

  const gun = () => TIERS[Math.min(tier, TIERS.length - 1)];
  const leftLimit = () => (gun().units > 1 ? 6 + POD_OFFSET : 6);
  const podX = () => playerX - POD_OFFSET;

  function promote(): void {
    if (tier < TIERS.length - 1) {
      tier++;
      cues.push("power");
    }
  }

  function spawn(kind: MiteKind, cx: number, cy: number): Mite {
    return {
      cx, cy, kind, hp: MITES[kind].hp,
      alive: true, hurt: 0, diving: false, dx: 0, dy: 0, fx: 0, fy: 0,
    };
  }

  function buildWave(n: number): void {
    const spec = L.waves[Math.min(n, L.waves.length - 1)];
    mites = [];
    originX = Math.round((VIEW_W - spec.cols * 26) / 2) + 4;
    originY = spec.hive ? 34 : 26;
    marchDir = 1;
    for (let r = 0; r < spec.rows.length; r++) {
      const kind = spec.rows[r] as MiteKind;
      for (let c = 0; c < spec.cols; c++) mites.push(spawn(kind, c * 26, r * 19));
    }
    // The hive rides above the wall, in the formation's own coordinates so it
    // marches with everything else.
    if (spec.hive) mites.push(spawn("hive", Math.round(((spec.cols - 1) * 26) / 2), -22));
    glitches.length = 0;
    bolts.length = 0;
    drops.length = 0;
    cooldown = 0;
    fireTimer = 0.9;
    diveTimer = 4.5;
    divesLeft = L.divers;
    hiveTimer = 5;
  }

  buildWave(0);

  const aliveMites = () => mites.filter((m) => m.alive);

  function marchSpeed(): number {
    const alive = aliveMites().length;
    const ratio = alive / Math.max(1, mites.length);
    return 14 + wave * 7 + (1 - ratio) * 30;
  }

  /** Where a mite actually is, formation or free flight. */
  const miteX = (m: Mite) => (m.diving ? m.fx : originX + m.cx);
  const miteY = (m: Mite) => (m.diving ? m.fy : originY + m.cy);

  function hurt(m: Mite, damage: number): boolean {
    m.hp -= damage;
    if (m.hp > 0) {
      m.hurt = 0.09;
      cues.push("clank");
      return false;
    }
    m.alive = false;
    score += MITES[m.kind].points;
    cues.push("hit");
    if (rand() < 0.16) {
      drops.push({
        x: miteX(m) + 1, y: miteY(m), live: true,
        kind: rand() < 0.65 ? "power" : "bomb",
      });
    }
    return true;
  }

  return {
    get status() { return status; },
    get score() { return score; },
    readout: () =>
      `WAVE ${Math.min(wave + 1, L.waves.length)}/${L.waves.length} · ${gun().name} · BOMBS ${bombs}`,
    update(dt, input) {
      if (status !== "playing") return;
      if (bombFlash > 0) bombFlash = Math.max(0, bombFlash - dt);

      if (interlude > 0) {
        interlude = Math.max(0, interlude - dt);
        if (interlude === 0) buildWave(wave);
        return;
      }

      const speed = 128;
      if (input.held.has("left")) playerX -= speed * dt;
      if (input.held.has("right")) playerX += speed * dt;
      playerX = Math.max(leftLimit(), Math.min(VIEW_W - 6 - MITE_W, playerX));

      // Held OR tapped: at RAPID and above the trigger is something you lean
      // on, and the cooldown rather than the keyboard sets the rate.
      cooldown = Math.max(0, cooldown - dt);
      const g = gun();
      const liveBolts = bolts.reduce((n, b) => n + (b.live ? 1 : 0), 0);
      if ((input.held.has("fire") || input.tapped.has("fire")) && cooldown === 0 && liveBolts < g.maxShots) {
        const muzzles = g.units > 1 ? [playerX + 3, podX() + 3] : [playerX + 3];
        for (const mx of muzzles) bolts.push({ x: mx, y: PLAYER_Y - 4, live: true, pierce: g.pierce });
        cooldown = g.cooldown;
        cues.push("shoot");
      }

      if (input.tapped.has("bomb") && bombs > 0) {
        bombs--;
        bombFlash = 0.45;
        cues.push("boom");
        for (const gl of glitches) gl.live = false;
        const live = aliveMites();
        if (live.length) {
          const front = Math.max(...live.map((m) => miteY(m)));
          for (const m of live) if (miteY(m) >= front - 20) hurt(m, 99);
        }
      }

      for (const b of bolts) {
        if (!b.live) continue;
        b.y -= g.speed * dt;
        if (b.y < -8) b.live = false;
      }
      if (bolts.length > 32) {
        for (let i = bolts.length - 1; i >= 0; i--) if (!bolts[i].live) bolts.splice(i, 1);
      }

      for (const d of drops) {
        if (!d.live) continue;
        d.y += 58 * dt;
        if (d.y > VIEW_H) d.live = false;
        else if (aabb(d.x, d.y, 6, 6, playerX - 4, PLAYER_Y - 2, MITE_W + 8, 10)) {
          d.live = false;
          score += 30;
          if (d.kind === "power") promote();
          else { bombs = Math.min(MAX_BOMBS, bombs + 1); cues.push("power"); }
        }
      }

      const alive = aliveMites();
      if (alive.length === 0) {
        wave++;
        score += 120;
        cues.push("clear");
        if (wave >= L.waves.length) { status = "cleared"; return; }
        promote();
        bombs = Math.min(MAX_BOMBS, bombs + 1);
        interlude = 1.1;
        return;
      }

      // Formation march, tested against the live extent so a cleared outer
      // column immediately buys the survivors more room. Divers are excluded:
      // they are no longer part of the wall.
      const inFormation = alive.filter((m) => !m.diving);
      if (inFormation.length) {
        const dx = marchSpeed() * marchDir * dt;
        const lo = Math.min(...inFormation.map((m) => m.cx));
        const hi = Math.max(...inFormation.map((m) => m.cx + MITES[m.kind].w));
        if (originX + lo + dx < 6 || originX + hi + dx > VIEW_W - 6) {
          marchDir *= -1;
          originY += 9;
        } else {
          originX += dx;
        }
      }

      animAcc += dt;
      if (animAcc > 0.28) { animAcc = 0; animFrame ^= 1; }

      // A living hive keeps making grubs, so the wall refills while you clear
      // it. Killing the hive is the only way the wave ever ends.
      const hive = alive.find((m) => m.kind === "hive");
      if (hive) {
        hiveTimer -= dt;
        if (hiveTimer <= 0) {
          hiveTimer = 4.5;
          const spots = [-26, 0, 26].map((o) => hive.cx + o);
          const free = spots.filter(
            (cx) => !mites.some((m) => m.alive && m.cx === cx && m.cy === 0),
          );
          if (free.length) {
            mites.push(spawn("grub", free[Math.floor(rand() * free.length)], 0));
            cues.push("clank");
          }
        }
      }

      // A darter peels off and comes at you. It aims once, at the moment it
      // leaves, so it is dodged by moving rather than by reflex.
      if (divesLeft > 0) {
        diveTimer -= dt;
        if (diveTimer <= 0) {
          const pool = inFormation.filter((m) => m.kind === "darter");
          if (pool.length) {
            const pick = pool[Math.floor(rand() * pool.length)];
            pick.diving = true;
            pick.fx = originX + pick.cx;
            pick.fy = originY + pick.cy;
            const aimX = playerX + 4 - (pick.fx + 4);
            const len = Math.max(1, Math.hypot(aimX, PLAYER_Y - pick.fy));
            pick.dx = (aimX / len) * 96;
            pick.dy = (Math.abs(PLAYER_Y - pick.fy) / len) * 96;
            divesLeft--;
            cues.push("clank");
          }
          diveTimer = 3.5 + rand() * 3;
        }
      }

      for (const m of alive) {
        if (!m.diving) continue;
        m.fx += m.dx * dt;
        m.fy += m.dy * dt;
        if (m.fy > VIEW_H + 8) { m.alive = false; continue; }
        if (aabb(m.fx, m.fy, MITES[m.kind].w, MITES[m.kind].h, playerX, PLAYER_Y, MITE_W, 4)) {
          status = "failed";
          cues.push("die");
          return;
        }
      }
      for (const m of mites) if (m.hurt > 0) m.hurt = Math.max(0, m.hurt - dt);

      // Return fire. A spitter's higher rate is expressed as a weighted draw,
      // so a wall thick with spitters really is louder.
      fireTimer -= dt;
      if (fireTimer <= 0) {
        fireTimer = Math.max(0.34, L.fireBase - wave * 0.22) * (0.6 + rand() * 0.9);
        const shooters = alive.filter((m) => !m.diving);
        if (shooters.length) {
          let total = 0;
          for (const m of shooters) total += MITES[m.kind].fire;
          let roll = rand() * total;
          let pick = shooters[0];
          for (const m of shooters) {
            roll -= MITES[m.kind].fire;
            if (roll <= 0) { pick = m; break; }
          }
          // Fire from the lowest live mite in that column, so a shot never
          // appears out of the middle of the wall.
          const column = shooters.filter((m) => m.cx === pick.cx);
          const shooter = column.reduce((a, b) => (b.cy > a.cy ? b : a), column[0]);
          const spec = MITES[shooter.kind];
          glitches.push({
            x: originX + shooter.cx + Math.floor(spec.w / 2) - 1,
            y: originY + shooter.cy + spec.h,
            live: true,
          });
        }
      }

      for (const gl of glitches) {
        if (!gl.live) continue;
        gl.y += 86 * dt;
        if (gl.y > VIEW_H) gl.live = false;
        else if (aabb(gl.x, gl.y, 2, 5, playerX, PLAYER_Y, MITE_W, 4)) {
          status = "failed";
          cues.push("die");
          return;
        }
      }

      for (const b of bolts) {
        if (!b.live) continue;
        for (const m of mites) {
          if (!m.alive) continue;
          const spec = MITES[m.kind];
          if (!aabb(b.x, b.y, g.width, 6, miteX(m), miteY(m), spec.w, spec.h)) continue;
          hurt(m, 1);
          b.pierce--;
          if (b.pierce <= 0) { b.live = false; break; }
        }
      }

      for (const m of alive) {
        if (m.diving) continue;
        if (originY + m.cy + MITES[m.kind].h >= PLAYER_Y) {
          status = "failed";
          cues.push("die");
          return;
        }
      }
    },
    draw(g) {
      bg.draw(g);
      g.fillStyle = IN.dusk;
      g.fillRect(0, VIEW_H - 12, VIEW_W, 2);

      for (const m of mites) {
        if (!m.alive) continue;
        const maps = SPRITES[m.kind];
        const map = maps[m.diving ? 1 : animFrame];
        const colour = m.hurt > 0 ? "#ffffff" : MITES[m.kind].colour;
        drawSprite(g, `${m.kind}${m.diving ? 1 : animFrame}${colour}`, map, { "#": colour }, miteX(m), miteY(m));
      }

      g.fillStyle = IN.hot;
      for (const gl of glitches) {
        if (!gl.live) continue;
        g.fillRect(Math.round(gl.x), Math.round(gl.y), 2, 3);
        g.fillRect(Math.round(gl.x), Math.round(gl.y) + 4, 2, 2);
      }

      for (const d of drops) {
        if (!d.live) continue;
        const dx = Math.round(d.x);
        const dy = Math.round(d.y);
        g.fillStyle = d.kind === "power" ? IN.gold : IN.hot;
        g.fillRect(dx, dy, 6, 6);
        g.fillStyle = IN.ink;
        if (d.kind === "power") {
          g.fillRect(dx + 2, dy + 1, 2, 4);
          g.fillRect(dx + 1, dy + 2, 4, 2);
        } else {
          g.fillRect(dx + 1, dy + 2, 4, 3);
        }
      }

      const wep = gun();
      g.fillStyle = wep.pierce > 1 ? IN.gold : IN.pale;
      const boltH = wep.pierce > 1 ? 8 : 6;
      for (const b of bolts) {
        if (!b.live) continue;
        g.fillRect(Math.round(b.x), Math.round(b.y), wep.width, boltH);
      }

      drawSprite(g, `cannon${IN.pale}`, CANNON, { "#": IN.pale }, playerX, PLAYER_Y);
      g.fillStyle = IN.wire;
      g.fillRect(Math.round(playerX), PLAYER_Y + 4, MITE_W, 2);

      if (wep.units > 1) {
        const px = Math.round(podX());
        g.fillStyle = IN.wire;
        g.fillRect(px + 2, PLAYER_Y, 4, 2);
        g.fillRect(px + 1, PLAYER_Y + 2, 6, 4);
        g.fillStyle = IN.ink;
        g.fillRect(px + 3, PLAYER_Y + 3, 2, 2);
      }

      if (bombFlash > 0) {
        g.fillStyle = `rgba(189, 243, 255, ${(bombFlash / 0.45) * 0.55})`;
        g.fillRect(0, 0, VIEW_W, VIEW_H);
      }
    },
    drain() { return cues.splice(0, cues.length); },
  };
}

/* -------------------------------------------------------------------------- */
/* Game 3: CODE MAZE                                                           */
/* -------------------------------------------------------------------------- */

const WALK = 11;

/** Per level: keys, seconds, and the hunters that walk the corridors. */
const MAZE_LEVELS = [
  { keys: 3, time: 88, hunters: ["patrol", "patrol"], speed: 39, breaks: 3, locks: 0 },
  { keys: 4, time: 94, hunters: ["patrol", "stalker"], speed: 44, breaks: 3, locks: 2 },
  // Two at the last level, not three. Three hunters in a braided maze is about
  // one per forty open tiles, and with contact instantly fatal that is density
  // rather than difficulty: you die to geometry, not to a mistake. A stalker
  // and a sweeper are more dangerous than three patrols and leave room to move.
  { keys: 5, time: 105, hunters: ["stalker", "sweeper"], speed: 48, breaks: 4, locks: 3 },
] as const;

/** How long a contended lock stays shut (and then open). */
const LOCK_PERIOD = 3.2;

/** How long a breakpoint holds. */
const FREEZE_TIME = 3;
/** Hunters stand still for this long at the start of a level, so a bad spawn
 *  cannot end a run before the player has taken a step. */
const SPAWN_GRACE = 1.8;

/** Nothing hunting you may match this. A chaser that is simply faster turns a
 *  corridor into a death you can see coming and cannot do anything about, so
 *  every hunter is capped a clear margin below it and outrunning one down a
 *  straight is always an option. */
const PLAYER_SPEED = 70;
const HUNTER_MAX_SPEED = PLAYER_SPEED - 12;

type HunterKind = "patrol" | "stalker" | "sweeper";

interface Hunter {
  x: number;
  y: number;
  tx: number;
  ty: number;
  dx: number;
  dy: number;
  kind: HunterKind;
}

const DIRS: readonly [number, number][] = [[1, 0], [-1, 0], [0, 1], [0, -1]];

/**
 * A maze that is different every run, three kinds of hunter that want
 * different things, and one idea of its own: the BREAKPOINT.
 *
 * Hitting it freezes every hunter where it stands for a couple of seconds,
 * which is the whole game in miniature. You are a cursor in a program, the
 * hunters are running code, and setting a breakpoint stops the program. It is
 * a limited resource, it recharges on the keys you collect, and it turns a
 * corridor you could not enter into one you can.
 */
function createCodeMaze(seed: number, level: number): StageRun {
  const L = MAZE_LEVELS[lvl(level)];
  const rand = makeRandom(seed);

  /* ---- carve a maze --------------------------------------------------- */

  // A fresh maze per run. One hand-authored map memorised in three attempts is
  // not a maze, it is a corridor with extra steps.
  const wall: boolean[][] = [];
  for (let y = 0; y < ROWS; y++) wall.push(new Array(COLS).fill(true));

  // Randomised depth-first carve over odd coordinates.
  const inGrid = (x: number, y: number) => x > 0 && y > 0 && x < COLS - 1 && y < ROWS - 1;
  const stack: Cell[] = [{ x: 1, y: 1 }];
  wall[1][1] = false;
  while (stack.length) {
    const c = stack[stack.length - 1];
    const options: Cell[] = [];
    for (const [dx, dy] of DIRS) {
      const nx = c.x + dx * 2;
      const ny = c.y + dy * 2;
      if (inGrid(nx, ny) && wall[ny][nx]) options.push({ x: nx, y: ny });
    }
    if (!options.length) { stack.pop(); continue; }
    const pick = options[Math.floor(rand() * options.length)];
    wall[(c.y + pick.y) / 2][(c.x + pick.x) / 2] = false;
    wall[pick.y][pick.x] = false;
    stack.push(pick);
  }

  // Braid it: knock holes in dead ends. A perfect maze is a death trap when
  // something is chasing you, because every wrong turn is a box.
  for (let y = 1; y < ROWS - 1; y += 2) {
    for (let x = 1; x < COLS - 1; x += 2) {
      if (wall[y][x]) continue;
      const open = DIRS.filter(([dx, dy]) => !wall[y + dy][x + dx]);
      if (open.length > 1) continue;
      const blocked = DIRS.filter(([dx, dy]) => inGrid(x + dx * 2, y + dy * 2) && wall[y + dy][x + dx]);
      if (blocked.length && rand() < 0.85) {
        const [dx, dy] = blocked[Math.floor(rand() * blocked.length)];
        wall[y + dy][x + dx] = false;
      }
    }
  }

  const solid = (tx: number, ty: number): boolean =>
    tx < 0 || ty < 0 || tx >= COLS || ty >= ROWS || wall[ty][tx];

  /* ---- place everything against a reachability pass -------------------- */

  const spawn = { x: 1, y: 1 };
  const idx = (x: number, y: number) => y * COLS + x;
  const reach: Cell[] = [];
  const dist = new Map<number, number>();
  const seen = new Set<number>([idx(spawn.x, spawn.y)]);
  dist.set(idx(spawn.x, spawn.y), 0);
  const q: Cell[] = [spawn];
  while (q.length) {
    const c = q.shift() as Cell;
    reach.push(c);
    const d = dist.get(idx(c.x, c.y)) ?? 0;
    for (const [dx, dy] of DIRS) {
      const nx = c.x + dx;
      const ny = c.y + dy;
      if (solid(nx, ny) || seen.has(idx(nx, ny))) continue;
      seen.add(idx(nx, ny));
      dist.set(idx(nx, ny), d + 1);
      q.push({ x: nx, y: ny });
    }
  }

  const gate = reach.reduce((a, b) =>
    (dist.get(idx(b.x, b.y)) ?? 0) > (dist.get(idx(a.x, a.y)) ?? 0) ? b : a,
  );

  const keys: Cell[] = [];
  const anchors: Cell[] = [spawn, gate];
  const spread = (c: Cell) => Math.min(...anchors.map((a) => Math.abs(a.x - c.x) + Math.abs(a.y - c.y)));
  const pool = reach.filter((c) => !anchors.some((a) => a.x === c.x && a.y === c.y));
  for (let i = 0; i < L.keys && pool.length; i++) {
    const best = pool.reduce((a, b) => (spread(b) > spread(a) ? b : a));
    keys.push(best);
    anchors.push(best);
    pool.splice(pool.indexOf(best), 1);
  }
  const taken = keys.map(() => false);

  // Contended locks: corridor tiles that alternate shut and open in two banks,
  // so at any moment one bank is passable. Only ever placed on a tile with
  // exactly two open neighbours (a corridor, never a junction), which is what
  // makes waiting for one a decision rather than a dead end.
  const locks: { x: number; y: number; bank: 0 | 1 }[] = [];
  {
    const corridors = reach.filter((c) => {
      if (c.x === spawn.x && c.y === spawn.y) return false;
      if (c.x === gate.x && c.y === gate.y) return false;
      if ((dist.get(idx(c.x, c.y)) ?? 0) < 4) return false;
      return DIRS.filter(([dx, dy]) => !solid(c.x + dx, c.y + dy)).length === 2;
    });
    for (let i = 0; i < L.locks && corridors.length; i++) {
      const pick = corridors.splice(Math.floor(rand() * corridors.length), 1)[0];
      locks.push({ x: pick.x, y: pick.y, bank: (i % 2) as 0 | 1 });
    }
  }
  let lockClock = 0;
  /** Bank 0 is shut for the first half of the cycle, bank 1 for the second. */
  const bankShut = (bank: 0 | 1) => (lockClock % (LOCK_PERIOD * 2) < LOCK_PERIOD ? bank === 0 : bank === 1);
  const lockShutAt = (tx: number, ty: number): boolean =>
    locks.some((l) => l.x === tx && l.y === ty && bankShut(l.bank));
  /** Wall, or a lock that happens to be shut right now. */
  const blocked = (tx: number, ty: number): boolean => solid(tx, ty) || lockShutAt(tx, ty);

  const hunters: Hunter[] = [];
  // Well away from the spawn, and further with every extra hunter: three of
  // them starting six tiles out means one is always already on top of you.
  const minDist = 8 + L.hunters.length * 2;
  const farEnough = reach.filter((c) => (dist.get(idx(c.x, c.y)) ?? 0) > minDist);
  const hunterPool = farEnough.length >= L.hunters.length
    ? farEnough
    : reach.filter((c) => (dist.get(idx(c.x, c.y)) ?? 0) > 6);
  for (const kind of L.hunters) {
    if (!hunterPool.length) break;
    const pick = hunterPool[Math.floor(rand() * hunterPool.length)];
    hunterPool.splice(hunterPool.indexOf(pick), 1);
    hunters.push({
      x: pick.x * CELL + 2, y: pick.y * CELL + 2,
      tx: pick.x, ty: pick.y, dx: 0, dy: 0, kind: kind as HunterKind,
    });
  }

  /* ---- the maze is static, so paint it once ---------------------------- */

  const bg = new StaticLayer(VIEW_W, VIEW_H, (g) => {
    g.fillStyle = IN.ink;
    g.fillRect(0, 0, VIEW_W, VIEW_H);
    for (let y = 0; y < ROWS; y++) {
      for (let x = 0; x < COLS; x++) {
        if (!wall[y][x]) continue;
        const px = x * CELL;
        const py = y * CELL;
        g.fillStyle = IN.grid;
        g.fillRect(px, py, CELL, CELL);
        g.fillStyle = IN.dusk;
        g.fillRect(px + 1, py + 1, CELL - 2, 1);
        g.fillRect(px + 1, py + CELL - 2, CELL - 2, 1);
      }
    }
  });

  /* ---- state ----------------------------------------------------------- */

  let px = spawn.x * CELL + 2;
  let py = spawn.y * CELL + 2;
  let clock = L.time;
  let status: RunStatus = "playing";
  let score = 0;
  let gateFlash = 0;
  // Annotated: the level table is `as const`, so an inferred type here would be
  // the literal 2 and every recharge below would be a type error.
  let breaks: number = L.breaks;
  let frozen = SPAWN_GRACE;
  const cues: Cue[] = [];
  /** Where you have been, so a maze you have not memorised is still navigable. */
  const trail = new Set<number>();

  const originOf = (t: number) => t * CELL + Math.floor((CELL - WALK) / 2);

  function clear(x: number, y: number): boolean {
    return (
      !blocked(Math.floor(x / CELL), Math.floor(y / CELL)) &&
      !blocked(Math.floor((x + WALK - 1) / CELL), Math.floor(y / CELL)) &&
      !blocked(Math.floor(x / CELL), Math.floor((y + WALK - 1) / CELL)) &&
      !blocked(Math.floor((x + WALK - 1) / CELL), Math.floor((y + WALK - 1) / CELL))
    );
  }

  /** Axes one at a time, with corner forgiveness: a corridor is 16px and the
   *  walker is 11, so a turn is only legal inside a 5px window, and asking a
   *  player to find that window while something is chasing them is how a maze
   *  stops being fun. */
  function move(x: number, y: number, dx: number, dy: number): [number, number] {
    const SLACK = CELL - WALK;
    let nx = x;
    let ny = y;
    if (dx !== 0) {
      if (clear(x + dx, ny)) nx = x + dx;
      else {
        const ay = originOf(Math.round((ny - originOf(0)) / CELL));
        if (Math.abs(ay - ny) <= SLACK && clear(x + dx, ay)) { nx = x + dx; ny = ay; }
      }
    }
    if (dy !== 0) {
      if (clear(nx, ny + dy)) ny = ny + dy;
      else {
        const ax = originOf(Math.round((nx - originOf(0)) / CELL));
        if (Math.abs(ax - nx) <= SLACK && clear(ax, ny + dy)) { nx = ax; ny = ny + dy; }
      }
    }
    return [nx, ny];
  }

  /** Each hunter picks its next tile its own way. The three read differently
   *  on screen, which is the point: you learn to treat them differently. */
  function pickTarget(h: Hunter): boolean {
    // Hunters respect the locks too, and re-decide at a tile centre, so a shut
    // lock turns one back rather than trapping it inside geometry.
    const options = DIRS.filter(([ox, oy]) => !blocked(h.tx + ox, h.ty + oy));
    if (!options.length) return false;
    const forward = options.filter(([ox, oy]) => !(ox === -h.dx && oy === -h.dy));
    const choices = forward.length ? forward : options;
    const toward = () =>
      choices.reduce((a, b) => {
        const cost = ([ox, oy]: readonly [number, number]) =>
          Math.abs((h.tx + ox) * CELL - px) + Math.abs((h.ty + oy) * CELL - py);
        return cost(b) < cost(a) ? b : a;
      });

    let pick: readonly [number, number];
    if (h.kind === "stalker") {
      // Comes for you, but not perfectly: a flawless chaser is a wall.
      pick = rand() < 0.6 ? toward() : choices[Math.floor(rand() * choices.length)];
    } else if (h.kind === "sweeper") {
      // Holds its line until the wall stops it. Fast and readable.
      const straight = choices.find(([ox, oy]) => ox === h.dx && oy === h.dy);
      pick = straight ?? choices[Math.floor(rand() * choices.length)];
    } else {
      pick = rand() < 0.25 ? toward() : choices[Math.floor(rand() * choices.length)];
    }
    h.dx = pick[0];
    h.dy = pick[1];
    h.tx += pick[0];
    h.ty += pick[1];
    return true;
  }

  const speedOf = (h: Hunter) =>
    Math.min(HUNTER_MAX_SPEED, h.kind === "sweeper" ? L.speed * 1.15 : L.speed);

  return {
    get status() { return status; },
    get score() { return score; },
    readout: () =>
      `KEYS ${taken.filter(Boolean).length}/${L.keys} · BREAK ${breaks} · ${Math.ceil(clock)}s`,
    update(dt, input) {
      if (status !== "playing") return;
      if (gateFlash > 0) gateFlash = Math.max(0, gateFlash - dt);
      if (frozen > 0) frozen = Math.max(0, frozen - dt);
      // Locks keep cycling through a breakpoint: the freeze stops the hunters,
      // not the clock, so hiding behind one still costs you the wait.
      lockClock += dt;

      clock -= dt;
      if (clock <= 0) { status = "failed"; cues.push("die"); return; }

      // The breakpoint. Everything running stops where it stands.
      if (input.tapped.has("bomb") && breaks > 0 && frozen <= 0) {
        breaks--;
        frozen = FREEZE_TIME;
        cues.push("boom");
      }

      const speed = PLAYER_SPEED * dt;
      let dx = 0;
      let dy = 0;
      if (input.held.has("left")) dx -= speed;
      if (input.held.has("right")) dx += speed;
      if (input.held.has("up")) dy -= speed;
      if (input.held.has("down")) dy += speed;
      [px, py] = move(px, py, dx, dy);
      trail.add(idx(Math.floor((px + WALK / 2) / CELL), Math.floor((py + WALK / 2) / CELL)));

      keys.forEach((k, i) => {
        if (taken[i]) return;
        if (!aabb(px, py, WALK, WALK, k.x * CELL + 4, k.y * CELL + 4, 8, 8)) return;
        taken[i] = true;
        score += 60;
        // Keys recharge the breakpoint, so pressing on is what refills it.
        breaks = Math.min(L.breaks + 1, breaks + 1);
        cues.push("pickup");
        if (taken.every(Boolean)) { gateFlash = 0.9; cues.push("gate"); }
      });

      if (frozen <= 0) {
        for (const h of hunters) {
          let budget = speedOf(h) * dt;
          for (let guard = 0; guard < 4 && budget > 0.0001; guard++) {
            const gx = originOf(h.tx);
            const gy = originOf(h.ty);
            const remaining = Math.abs(gx - h.x) + Math.abs(gy - h.y);
            if (remaining <= budget) {
              h.x = gx;
              h.y = gy;
              budget -= remaining;
              if (!pickTarget(h)) break;
            } else {
              h.x += Math.sign(gx - h.x) * Math.min(budget, Math.abs(gx - h.x));
              h.y += Math.sign(gy - h.y) * Math.min(budget, Math.abs(gy - h.y));
              budget = 0;
            }
          }
        }
      }
      // Contact needs real overlap, not a shared pixel. Both boxes are inset,
      // so meeting a hunter head-on in a corridor still kills you (there is
      // nowhere to be) while clipping a corner as one crosses a junction does
      // not. Without this, an 11px box in a 16px corridor makes every near
      // miss fatal, and the level reads as unfair rather than hard.
      const GRAZE = 3;
      const hitW = WALK - GRAZE * 2;
      for (const h of hunters) {
        if (aabb(px + GRAZE, py + GRAZE, hitW, hitW, h.x + GRAZE, h.y + GRAZE, hitW, hitW)) {
          status = "failed";
          cues.push("die");
          return;
        }
      }

      if (taken.every(Boolean) && aabb(px, py, WALK, WALK, gate.x * CELL + 2, gate.y * CELL + 2, 12, 12)) {
        status = "cleared";
        score += 150 + Math.round(clock) * 3;
        cues.push("clear");
      }
    },
    draw(g) {
      bg.draw(g);

      // Where you have been, faintly. Reading your own path back is half of
      // finding your way out of somewhere you have never seen before.
      g.fillStyle = "rgba(45, 225, 194, 0.09)";
      for (const t of trail) {
        const tx = t % COLS;
        const ty = (t - tx) / COLS;
        g.fillRect(tx * CELL + 5, ty * CELL + 5, 6, 6);
      }

      // Locks: shut ones are barred and solid, open ones are a faint frame so
      // you can see where the next one will close on you.
      for (const l of locks) {
        const lx = l.x * CELL;
        const ly = l.y * CELL;
        if (bankShut(l.bank)) {
          g.fillStyle = IN.rust;
          g.fillRect(lx + 1, ly + 1, CELL - 2, CELL - 2);
          g.fillStyle = IN.ink;
          for (let i = 0; i < 3; i++) g.fillRect(lx + 3 + i * 4, ly + 2, 2, CELL - 4);
        } else {
          g.fillStyle = "rgba(255, 138, 61, 0.22)";
          g.fillRect(lx + 1, ly + 1, CELL - 2, 1);
          g.fillRect(lx + 1, ly + CELL - 2, CELL - 2, 1);
        }
      }

      const open = taken.every(Boolean);
      const gx = gate.x * CELL;
      const gy = gate.y * CELL;
      g.fillStyle = open ? (gateFlash > 0 ? IN.pale : IN.wire) : IN.wireDim;
      g.fillRect(gx + 2, gy + 2, 12, 12);
      g.fillStyle = IN.ink;
      if (open) g.fillRect(gx + 5, gy + 5, 6, 6);
      else for (let i = 0; i < 3; i++) g.fillRect(gx + 4 + i * 4, gy + 3, 2, 10);

      g.fillStyle = IN.gold;
      keys.forEach((k, i) => {
        if (taken[i]) return;
        g.fillRect(k.x * CELL + 5, k.y * CELL + 4, 6, 4);
        g.fillRect(k.x * CELL + 7, k.y * CELL + 8, 2, 4);
        g.fillRect(k.x * CELL + 7, k.y * CELL + 10, 4, 2);
      });

      // Hunters. Frozen ones go pale blue and grow a pause bar, so the state
      // is legible at a glance rather than inferred from them not moving.
      for (const h of hunters) {
        const hx = Math.round(h.x);
        const hy = Math.round(h.y);
        g.fillStyle = frozen > 0 ? "#6ff2ff" : h.kind === "stalker" ? IN.hot : h.kind === "sweeper" ? IN.rust : IN.violet;
        g.fillRect(hx, hy, WALK, WALK);
        g.fillStyle = IN.ink;
        if (frozen > 0) {
          g.fillRect(hx + 3, hy + 3, 2, 5);
          g.fillRect(hx + 6, hy + 3, 2, 5);
        } else {
          g.fillRect(hx + 2, hy + 3, 2, 3);
          g.fillRect(hx + 7, hy + 3, 2, 3);
        }
      }

      g.fillStyle = IN.pale;
      g.fillRect(Math.round(px) + 3, Math.round(py), 5, WALK);
      g.fillRect(Math.round(px), Math.round(py), WALK, 2);
      g.fillRect(Math.round(px), Math.round(py) + WALK - 2, WALK, 2);

      const frac = Math.max(0, clock / L.time);
      g.fillStyle = frac < 0.25 ? IN.hot : IN.wire;
      g.fillRect(0, 0, Math.round(VIEW_W * frac), 2);
      if (frozen > 0) {
        g.fillStyle = "#6ff2ff";
        g.fillRect(0, 2, Math.round(VIEW_W * Math.min(1, frozen / FREEZE_TIME)), 1);
      }
    },
    drain() { return cues.splice(0, cues.length); },
  };
}

/* -------------------------------------------------------------------------- */

export const STAGES: readonly Stage[] = [
  {
    id: "knotline",
    title: "KNOTLINE",
    brief: "Weave the thread through the code nodes. Do not cross your own line.",
    controls: "ARROWS or WASD to steer",
    levelNote: (n) => {
      const L = KNOT_LEVELS[lvl(n)];
      return `${L.target} nodes, ${L.severed} severed circuits, and a thread that quickens with every one you take.${
        L.fault ? " A FAULT walks the board hunting your head. It does not stop." : ""
      }`;
    },
    create: createKnotline,
  },
  {
    id: "bug-blaster",
    title: "BUG BLASTER",
    brief:
      "Four kinds of mite are walking down the stack. Grubs die to anything, shells take two, spitters return fire hardest, and darters leave the wall and come for you.",
    controls: "LEFT / RIGHT to move · hold SPACE to fire · X for a bomb",
    levelNote: (n) => {
      const L = BLAST_LEVELS[lvl(n)];
      const kinds = new Set<string>();
      for (const w of L.waves) for (const r of w.rows) kinds.add(r);
      const hive = L.waves.some((w) => w.hive);
      return `${L.waves.length} waves of ${[...kinds].join(", ")}${L.divers ? `, with ${L.divers} diving run${L.divers > 1 ? "s" : ""}` : ""}. You start on ${TIERS[L.startTier].name}.${
        hive ? " A HIVE holds the last wave: ten hits, and it breeds while you shoot." : ""
      }`;
    },
    create: createBugBlaster,
  },
  {
    id: "code-maze",
    title: "CODE MAZE",
    brief:
      "A maze you have never seen, carved fresh every run. Collect every compiler key, then reach the gate. X sets a BREAKPOINT and freezes everything hunting you.",
    controls: "ARROWS or WASD to move · X for a breakpoint",
    levelNote: (n) => {
      const L = MAZE_LEVELS[lvl(n)];
      return `${L.keys} keys, ${L.time} seconds, ${L.hunters.length} hunters (${[...new Set(L.hunters)].join(", ")}), ${L.breaks} breakpoints.${
        L.locks ? ` ${L.locks} contended locks open and shut on their own.` : ""
      }`;
    },
    create: createCodeMaze,
  },
];
