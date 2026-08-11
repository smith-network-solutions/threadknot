// The three stages of the hidden circuit.
//
// Everything here is original: the mechanics, the sprites, the maze and the
// names. The debt this pays is to a *way of building things* (a fixed low
// resolution, a handful of colours, one screen at a time), not to any
// particular cabinet, and nothing in this file reproduces artwork, audio,
// characters or branding from one.
//
// The stages know nothing about React, sound or the CRT frame around them.
// Each is a factory returning a run with the same four moving parts: take
// input, advance by a timestep, draw into a 320x240 buffer, and raise cues the
// shell turns into noise. That is the whole contract, which is why a fourth
// stage would be a new entry in STAGES and nothing else.

/** Logical resolution. The canvas is this size and CSS scales it by a whole
 *  number, so a pixel is a pixel and nothing is ever half-lit. */
export const VIEW_W = 320;
export const VIEW_H = 240;

const CELL = 16;
const COLS = VIEW_W / CELL; // 20
const ROWS = VIEW_H / CELL; // 15

/** The cabinet palette. Deliberately small, and shared by all three stages so
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
  | "boom";

export type RunStatus = "playing" | "cleared" | "failed";

export interface StageRun {
  status: RunStatus;
  /** Points banked in this stage so far. Added to the campaign total when the
   *  stage is cleared, and thrown away when a life is lost, so a life is worth
   *  something beyond the counter. */
  score: number;
  /** The one line of stage-specific state the HUD shows. */
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
  /** Marquee name. */
  title: string;
  /** One line on the attract card: what you are trying to do. */
  brief: string;
  /** One line on the attract card: how to do it. */
  controls: string;
  /** What changes at this level, for the attract card. */
  levelNote(level: number): string;
  /** `level` is 0, 1 or 2. */
  create(seed: number, level: number): StageRun;
}

/** Clamp a level index coming from the campaign into the range a stage's own
 *  difficulty tables are sized for. */
function lvl(level: number): number {
  return Math.max(0, Math.min(LEVELS_PER_STAGE - 1, Math.floor(level)));
}

/* -------------------------------------------------------------------------- */
/* Small shared parts                                                          */
/* -------------------------------------------------------------------------- */

/** A seeded generator, so a stage plays out the same way for the same seed.
 *  Worth the six lines: "it only happens sometimes" is not a bug report anyone
 *  can act on, and a seed in the console makes it one that is. */
function makeRandom(seed: number): () => number {
  let s = (seed >>> 0) || 0x9e3779b9;
  return () => {
    // xorshift32
    s ^= s << 13;
    s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 0x100000000;
  };
}

/** Paint a pixel map (rows of "#"/"." characters) at logical pixel scale. */
function blit(
  g: CanvasRenderingContext2D,
  map: readonly string[],
  x: number,
  y: number,
  color: string,
  scale = 1,
): void {
  g.fillStyle = color;
  for (let r = 0; r < map.length; r++) {
    const row = map[r];
    for (let c = 0; c < row.length; c++) {
      if (row[c] !== ".") g.fillRect(x + c * scale, y + r * scale, scale, scale);
    }
  }
}

/** The faint back-of-the-tube grid every stage sits on. */
function drawField(g: CanvasRenderingContext2D): void {
  g.fillStyle = IN.ink;
  g.fillRect(0, 0, VIEW_W, VIEW_H);
  g.fillStyle = IN.grid;
  for (let x = CELL; x < VIEW_W; x += CELL) g.fillRect(x, 0, 1, VIEW_H);
  for (let y = CELL; y < VIEW_H; y += CELL) g.fillRect(0, y, VIEW_W, 1);
}

function aabb(
  ax: number,
  ay: number,
  aw: number,
  ah: number,
  bx: number,
  by: number,
  bw: number,
  bh: number,
): boolean {
  return ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by;
}

/* -------------------------------------------------------------------------- */
/* Stage 1: KNOTLINE                                                           */
/* -------------------------------------------------------------------------- */

interface Cell {
  x: number;
  y: number;
}

/** Per level: nodes to collect, starting step time, how much each node speeds
 *  the thread up, and how many severed cells eventually open. */
const KNOT_LEVELS = [
  { target: 6, step: 0.15, quicken: 0.006, severed: 2, floor: 0.085 },
  { target: 8, step: 0.132, quicken: 0.0068, severed: 3, floor: 0.075 },
  { target: 10, step: 0.118, quicken: 0.0072, severed: 5, floor: 0.066 },
] as const;

/**
 * A thread you are paying out across a board. It grows every time it reaches a
 * code node, and it is the only thing on the board that can kill you: the walls
 * of the frame, the severed circuits that open up once you are half way, and
 * the length of thread already behind you.
 */
function createKnotline(seed: number, level: number): StageRun {
  const L = KNOT_LEVELS[lvl(level)];
  const KNOT_TARGET = L.target;
  const rand = makeRandom(seed);
  let body: Cell[] = [
    { x: 6, y: 7 },
    { x: 5, y: 7 },
    { x: 4, y: 7 },
  ];
  let dir: Cell = { x: 1, y: 0 };
  let queued: Cell[] = [];
  let grow = 0;
  let taken = 0;
  let acc = 0;
  // Annotated: the level table is `as const`, so an inferred type here would be
  // the literal 0.15 and every speed-up below would be a type error.
  let stepMs: number = L.step;
  let node: Cell = { x: 13, y: 7 };
  const severed: Cell[] = [];
  const cues: Cue[] = [];
  let status: RunStatus = "playing";
  let score = 0;
  let flash = 0;

  const occupied = (c: Cell): boolean =>
    body.some((b) => b.x === c.x && b.y === c.y) ||
    severed.some((s) => s.x === c.x && s.y === c.y) ||
    (node.x === c.x && node.y === c.y);

  function freeCell(): Cell {
    // Bounded search, then a scan: on a board this small the random pick lands
    // almost immediately, but "almost" is not a loop condition.
    for (let i = 0; i < 200; i++) {
      const c = {
        x: 1 + Math.floor(rand() * (COLS - 2)),
        y: 1 + Math.floor(rand() * (ROWS - 2)),
      };
      if (!occupied(c)) return c;
    }
    for (let y = 1; y < ROWS - 1; y++) {
      for (let x = 1; x < COLS - 1; x++) {
        if (!occupied({ x, y })) return { x, y };
      }
    }
    return { x: 1, y: 1 };
  }

  node = freeCell();

  function step(): void {
    const next = queued.shift();
    // A queued turn is only honoured if it is not a reversal into your own
    // neck; without this, a fast left-then-down on a horizontal thread reads as
    // an instant death rather than the turn the player made.
    if (next && !(next.x === -dir.x && next.y === -dir.y)) dir = next;

    const head = { x: body[0].x + dir.x, y: body[0].y + dir.y };

    const hitWall = head.x <= 0 || head.y <= 0 || head.x >= COLS - 1 || head.y >= ROWS - 1;
    const hitSelf = body.some((b, i) => i < body.length - 1 && b.x === head.x && b.y === head.y);
    const hitSevered = severed.some((s) => s.x === head.x && s.y === head.y);
    if (hitWall || hitSelf || hitSevered) {
      status = "failed";
      cues.push("die");
      return;
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
      if (taken >= KNOT_TARGET) {
        status = "cleared";
        score += 100;
        cues.push("clear");
        return;
      }
      // The board closes in from the halfway mark: the same board for the whole
      // level would be over as a challenge by the fourth node.
      if (taken >= 2 && severed.length < L.severed) severed.push(freeCell());
      node = freeCell();
    } else {
      cues.push("step");
    }
  }

  return {
    get status() {
      return status;
    },
    get score() {
      return score;
    },
    readout: () => `NODES ${taken}/${KNOT_TARGET}`,
    update(dt, input) {
      if (status !== "playing") return;
      if (flash > 0) flash = Math.max(0, flash - dt);
      // Turns are buffered rather than read at step time, so an input between
      // two steps is never dropped: at eight steps a second, a dropped turn is
      // the difference between a game that feels tight and one that feels deaf.
      const push = (c: Cell) => {
        if (queued.length < 2) queued.push(c);
      };
      if (input.tapped.has("up")) push({ x: 0, y: -1 });
      if (input.tapped.has("down")) push({ x: 0, y: 1 });
      if (input.tapped.has("left")) push({ x: -1, y: 0 });
      if (input.tapped.has("right")) push({ x: 1, y: 0 });

      acc += dt;
      let guard = 8; // never let a stalled tab replay a minute of steps at once
      while (acc >= stepMs && status === "playing" && guard-- > 0) {
        acc -= stepMs;
        step();
      }
      if (guard <= 0) acc = 0;
    },
    draw(g) {
      drawField(g);

      // Frame.
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

      // Severed circuits: a broken X in a dead cell.
      for (const s of severed) {
        const px = s.x * CELL;
        const py = s.y * CELL;
        g.fillStyle = IN.hot;
        for (let i = 2; i < CELL - 2; i++) {
          g.fillRect(px + i, py + i, 2, 2);
          g.fillRect(px + (CELL - 2 - i), py + i, 2, 2);
        }
      }

      // Node.
      const np = flash > 0 ? IN.pale : IN.gold;
      g.fillStyle = np;
      g.fillRect(node.x * CELL + 4, node.y * CELL + 4, 8, 8);
      g.fillRect(node.x * CELL + 6, node.y * CELL + 1, 4, 2);
      g.fillRect(node.x * CELL + 6, node.y * CELL + 13, 4, 2);
      g.fillRect(node.x * CELL + 1, node.y * CELL + 6, 2, 4);
      g.fillRect(node.x * CELL + 13, node.y * CELL + 6, 2, 4);

      // Thread: the head reads brightest and the tail fades, so at speed you
      // can still tell which end you are steering.
      for (let i = body.length - 1; i >= 0; i--) {
        const b = body[i];
        const t = 1 - i / Math.max(1, body.length);
        g.fillStyle = i === 0 ? IN.pale : t > 0.45 ? IN.wire : IN.wireDim;
        const inset = i === 0 ? 2 : 3;
        g.fillRect(b.x * CELL + inset, b.y * CELL + inset, CELL - inset * 2, CELL - inset * 2);
      }
    },
    drain() {
      return cues.splice(0, cues.length);
    },
  };
}

/* -------------------------------------------------------------------------- */
/* Stage 2: BUG BLASTER                                                        */
/* -------------------------------------------------------------------------- */

/** An abstract mite: two brackets and four legs. Drawn from a pixel map so it
 *  has the hand-placed look of something authored one pixel at a time. */
const MITE_A = [
  "..#..#..",
  ".######.",
  "##.##.##",
  "########",
  "#.#..#.#",
] as const;

const MITE_B = [
  "#.#..#.#",
  ".######.",
  "##.##.##",
  "########",
  "..#..#..",
] as const;

const CANNON = ["...##...", "..####..", ".######.", "########"] as const;

const MITE_W = 8;
const MITE_H = 5;
/** Per level: the two waves it puts up, and how many rows deep they are.
 *  A level is a short fight rather than a marathon, so a lost life costs you a
 *  couple of waves and not the whole game. */
const BLAST_LEVELS = [
  { waves: [6, 7], rows: 3, startTier: 0, fireBase: 1.6 },
  { waves: [7, 8], rows: 3, startTier: 1, fireBase: 1.3 },
  // Four rows deep is already a lot of wall; pairing it with the fastest
  // return fire made the last level a coin toss rather than a hard fight.
  { waves: [8, 8], rows: 4, startTier: 2, fireBase: 1.3 },
] as const;
const PLAYER_Y = VIEW_H - 22;

interface Mite {
  cx: number;
  cy: number;
  alive: boolean;
}

interface Shot {
  x: number;
  y: number;
  live: boolean;
}

/** A bolt the player fired. `pierce` is how many more mites it may still go
 *  through before it is spent, which is the whole difference a missile makes. */
interface Bolt extends Shot {
  pierce: number;
}

/** Something a dying mite left behind, falling toward the cannon. */
interface Drop {
  x: number;
  y: number;
  live: boolean;
  kind: "power" | "bomb";
}

/** The gun ladder. Each rung is a real change in how the stage plays, not a
 *  number going up: more bolts in the air at once, a shorter gap between
 *  volleys, a second firing unit, and finally a shot that does not stop at the
 *  first thing it hits. */
const TIERS = [
  { name: "BOLT", maxShots: 2, cooldown: 0.2, speed: 340, pierce: 1, units: 1, width: 2 },
  { name: "RAPID", maxShots: 4, cooldown: 0.12, speed: 380, pierce: 1, units: 1, width: 2 },
  { name: "TWIN", maxShots: 6, cooldown: 0.12, speed: 380, pierce: 1, units: 2, width: 2 },
  { name: "MISSILE", maxShots: 8, cooldown: 0.09, speed: 460, pierce: 3, units: 2, width: 3 },
] as const;

const MAX_BOMBS = 3;
/** How far to the left of the cannon the support unit flies. */
const POD_OFFSET = 18;

/**
 * A wall of mites walking a descending grid while you hold the bottom of the
 * screen. You start with a peashooter and two shots in the air; clearing a wave
 * promotes the gun, and the mites themselves drop the rest. By the last wave a
 * well-armed player has two units firing piercing missiles and a bomb in
 * reserve, which is roughly what the wall has earned by then.
 */
function createBugBlaster(seed: number, level: number): StageRun {
  const L = BLAST_LEVELS[lvl(level)];
  const WAVE_COLS = L.waves;
  const WAVE_ROWS = L.rows;
  const rand = makeRandom(seed);
  let wave = 0;
  let mites: Mite[] = [];
  let originX = 0;
  let originY = 0;
  let marchDir = 1;
  let animAcc = 0;
  let animFrame = 0;
  let fireTimer = 0;
  let playerX = VIEW_W / 2 - 4;
  // Later levels hand you the gun you would have earned by then, so a level
  // three restart is not a level one restart with a level three wall.
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
  /** Left clamp has to leave room for the support unit, or the pod would fly
   *  off the side of the screen the moment you hug the left wall. */
  const leftLimit = () => (gun().units > 1 ? 6 + POD_OFFSET : 6);
  const podX = () => playerX - POD_OFFSET;

  function promote(): void {
    if (tier < TIERS.length - 1) {
      tier++;
      cues.push("power");
    }
  }

  function buildWave(n: number): void {
    const cols = WAVE_COLS[n];
    mites = [];
    originX = Math.round((VIEW_W - cols * 26) / 2) + 4;
    originY = 30;
    marchDir = 1;
    for (let r = 0; r < WAVE_ROWS; r++) {
      for (let c = 0; c < cols; c++) {
        mites.push({ cx: c * 26, cy: r * 20, alive: true });
      }
    }
    glitches.length = 0;
    bolts.length = 0;
    drops.length = 0;
    cooldown = 0;
    fireTimer = 0.9;
  }

  buildWave(0);

  const aliveMites = () => mites.filter((m) => m.alive);

  function marchSpeed(): number {
    const alive = aliveMites().length;
    const ratio = alive / Math.max(1, mites.length);
    // Thinning the wall speeds up what is left: the last mite on the board is
    // the one that has been running from you the longest.
    //
    // The ceiling is set by the one-shot-at-a-time rule rather than by taste.
    // A shot is in the air for about four tenths of a second, and if the wall
    // covers more than a sprite's width in that time then an aimed shot lands
    // in the gap and the stage becomes a lottery rather than a test.
    return 14 + wave * 7 + (1 - ratio) * 30;
  }

  return {
    get status() {
      return status;
    },
    get score() {
      return score;
    },
    readout: () =>
      `WAVE ${Math.min(wave + 1, WAVE_COLS.length)}/${WAVE_COLS.length} · ${gun().name} · BOMBS ${bombs}`,
    update(dt, input) {
      if (status !== "playing") return;

      if (bombFlash > 0) bombFlash = Math.max(0, bombFlash - dt);

      if (interlude > 0) {
        interlude = Math.max(0, interlude - dt);
        if (interlude === 0) buildWave(wave);
        return;
      }

      // Cannon.
      const speed = 128;
      if (input.held.has("left")) playerX -= speed * dt;
      if (input.held.has("right")) playerX += speed * dt;
      playerX = Math.max(leftLimit(), Math.min(VIEW_W - 6 - MITE_W, playerX));

      // Firing. Held OR tapped: at RAPID and above the trigger is something you
      // lean on, and forcing a tap per bolt would waste the cadence the upgrade
      // just bought. The cooldown, not the keyboard, sets the rate.
      cooldown = Math.max(0, cooldown - dt);
      const wantsFire = input.held.has("fire") || input.tapped.has("fire");
      const g = gun();
      if (wantsFire && cooldown === 0 && bolts.filter((b) => b.live).length < g.maxShots) {
        const muzzles = g.units > 1 ? [playerX + 3, podX() + 3] : [playerX + 3];
        for (const mx of muzzles) {
          bolts.push({ x: mx, y: PLAYER_Y - 4, live: true, pierce: g.pierce });
        }
        cooldown = g.cooldown;
        cues.push("shoot");
      }

      // The bomb: everything in the air dies, and so does the front of the wall.
      // One button, a scarce resource, and the answer to being boxed in.
      if (input.tapped.has("bomb") && bombs > 0) {
        bombs--;
        bombFlash = 0.45;
        cues.push("boom");
        for (const gl of glitches) gl.live = false;
        const live = mites.filter((m) => m.alive);
        if (live.length) {
          const front = Math.max(...live.map((m) => m.cy));
          for (const m of live) {
            if (m.cy >= front - 20) {
              m.alive = false;
              score += 20;
            }
          }
        }
      }

      for (const b of bolts) {
        if (!b.live) continue;
        b.y -= g.speed * dt;
        if (b.y < -8) b.live = false;
      }
      // Spent bolts are swept rather than left to grow the array for the whole
      // stage; the cap above counts live ones, so this is housekeeping only.
      if (bolts.length > 32) {
        for (let i = bolts.length - 1; i >= 0; i--) if (!bolts[i].live) bolts.splice(i, 1);
      }

      // Capsules fall toward the cannon; the catch box is a little wider than
      // the cannon, because missing a power-up by a pixel is not a lesson.
      for (const d of drops) {
        if (!d.live) continue;
        d.y += 58 * dt;
        if (d.y > VIEW_H) {
          d.live = false;
        } else if (aabb(d.x, d.y, 6, 6, playerX - 4, PLAYER_Y - 2, MITE_W + 8, 10)) {
          d.live = false;
          score += 30;
          if (d.kind === "power") promote();
          else {
            bombs = Math.min(MAX_BOMBS, bombs + 1);
            cues.push("power");
          }
        }
      }

      // Formation march. Edges are tested against the live extent, so a cleared
      // outer column immediately buys the survivors more room.
      const alive = aliveMites();
      if (alive.length === 0) {
        wave++;
        score += 120;
        cues.push("clear");
        if (wave >= WAVE_COLS.length) {
          status = "cleared";
          return;
        }
        // Clearing a wave always promotes the gun, so a player who caught no
        // capsules at all still walks into the next wave better armed. Drops
        // are the fast route, not the only one.
        promote();
        bombs = Math.min(MAX_BOMBS, bombs + 1);
        interlude = 1.1;
        return;
      }

      const dx = marchSpeed() * marchDir * dt;
      const minX = Math.min(...alive.map((m) => m.cx));
      const maxX = Math.max(...alive.map((m) => m.cx));
      const leftEdge = originX + minX + dx;
      const rightEdge = originX + maxX + MITE_W + dx;
      if (leftEdge < 6 || rightEdge > VIEW_W - 6) {
        marchDir *= -1;
        originY += 9;
      } else {
        originX += dx;
      }

      animAcc += dt;
      if (animAcc > 0.28) {
        animAcc = 0;
        animFrame ^= 1;
      }

      // Return fire, from the lowest live mite in a random column so a shot
      // never appears out of the middle of the wall.
      fireTimer -= dt;
      if (fireTimer <= 0) {
        fireTimer = Math.max(0.4, L.fireBase - wave * 0.25) * (0.6 + rand() * 0.9);
        const pick = alive[Math.floor(rand() * alive.length)];
        const column = alive.filter((m) => m.cx === pick.cx);
        const shooter = column.reduce((a, b) => (b.cy > a.cy ? b : a), column[0]);
        glitches.push({
          x: originX + shooter.cx + 3,
          y: originY + shooter.cy + MITE_H,
          live: true,
        });
      }

      for (const gl of glitches) {
        if (!gl.live) continue;
        // Slow enough that every hit is a hit you could have stepped out of.
        gl.y += 86 * dt;
        if (gl.y > VIEW_H) gl.live = false;
        else if (aabb(gl.x, gl.y, 2, 5, playerX, PLAYER_Y, MITE_W, 4)) {
          status = "failed";
          cues.push("die");
          return;
        }
      }

      // Bolts against the wall. A bolt with pierce left keeps going, which is
      // what makes a missile worth catching: one shot down a full column.
      for (const b of bolts) {
        if (!b.live) continue;
        for (const m of mites) {
          if (!m.alive) continue;
          const mx = originX + m.cx;
          const my = originY + m.cy;
          if (!aabb(b.x, b.y, g.width, 6, mx, my, MITE_W, MITE_H)) continue;
          m.alive = false;
          score += 15;
          cues.push("hit");
          if (rand() < 0.16) {
            drops.push({
              x: mx + 1,
              y: my,
              live: true,
              kind: rand() < 0.65 ? "power" : "bomb",
            });
          }
          b.pierce--;
          if (b.pierce <= 0) {
            b.live = false;
            break;
          }
        }
      }

      // The wall arriving is the other way to lose.
      for (const m of alive) {
        if (originY + m.cy + MITE_H >= PLAYER_Y) {
          status = "failed";
          cues.push("die");
          return;
        }
      }
    },
    draw(g) {
      drawField(g);

      // Ground line.
      g.fillStyle = IN.dusk;
      g.fillRect(0, VIEW_H - 12, VIEW_W, 2);

      const frame = animFrame === 0 ? MITE_A : MITE_B;
      for (const m of mites) {
        if (!m.alive) continue;
        const my = originY + m.cy;
        // Colour by row so depth in the wall is readable at a glance.
        const tone = m.cy === 0 ? IN.hot : m.cy === 20 ? IN.gold : IN.wire;
        blit(g, frame, Math.round(originX + m.cx), Math.round(my), tone);
      }

      for (const gl of glitches) {
        if (!gl.live) continue;
        g.fillStyle = IN.hot;
        g.fillRect(Math.round(gl.x), Math.round(gl.y), 2, 3);
        g.fillRect(Math.round(gl.x), Math.round(gl.y) + 4, 2, 2);
      }

      // Capsules: a framed box, gold for a gun, pink for a bomb, with a mark
      // inside so the two are told apart at a glance rather than by colour
      // alone.
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
      for (const b of bolts) {
        if (!b.live) continue;
        // A missile reads as a missile: wider, longer, and gold rather than white.
        g.fillStyle = wep.pierce > 1 ? IN.gold : IN.pale;
        g.fillRect(Math.round(b.x), Math.round(b.y), wep.width, wep.pierce > 1 ? 8 : 6);
      }

      blit(g, CANNON, Math.round(playerX), PLAYER_Y, IN.pale);
      g.fillStyle = IN.wire;
      g.fillRect(Math.round(playerX), PLAYER_Y + 4, MITE_W, 2);

      // The support unit, drawn a little smaller so the cannon you actually
      // steer (and the only one that can be hit) stays the obvious one.
      if (wep.units > 1) {
        const px = Math.round(podX());
        g.fillStyle = IN.wire;
        g.fillRect(px + 2, PLAYER_Y, 4, 2);
        g.fillRect(px + 1, PLAYER_Y + 2, 6, 4);
        g.fillStyle = IN.ink;
        g.fillRect(px + 3, PLAYER_Y + 3, 2, 2);
      }

      // Bomb flash: the whole tube whites out for a moment.
      if (bombFlash > 0) {
        g.fillStyle = `rgba(189, 243, 255, ${(bombFlash / 0.45) * 0.55})`;
        g.fillRect(0, 0, VIEW_W, VIEW_H);
      }
    },
    drain() {
      return cues.splice(0, cues.length);
    },
  };
}

/* -------------------------------------------------------------------------- */
/* Stage 3: CODE MAZE                                                          */
/* -------------------------------------------------------------------------- */

/** Walls, one spawn, one gate. Everything else the stage places itself against
 *  a reachability pass, so a hand-authored corner that turns out to be walled
 *  off can never strand a key behind it. */
const MAZE: readonly string[] = [
  "####################",
  "#P...#.........#..X#",
  "#.##.#.#####.#.##..#",
  "#..#...#...#.#..#..#",
  "##.#.###.#.#.##.#.##",
  "#..#.....#....#....#",
  "#.####.#####.####.##",
  "#......#.....#.....#",
  "##.####.#.#.####.#.#",
  "#....#..#.#..#...#.#",
  "#.##.#.##.##.#.###.#",
  "#.#...........#....#",
  "#.#.####.####.####.#",
  "#......#...........#",
  "####################",
];

/** Per level: keys to find, seconds on the clock, how many hunters walk the
 *  corridors, how fast they walk, and how often they lean toward you. */
const MAZE_LEVELS = [
  { keys: 2, time: 68, hunters: 2, speed: 44, hunt: 0.22 },
  { keys: 3, time: 74, hunters: 2, speed: 48, hunt: 0.28 },
  { keys: 4, time: 82, hunters: 3, speed: 53, hunt: 0.36 },
] as const;
const WALK = 11; // player/hunter box, a little under a tile so corners forgive

interface Walker {
  /** Pixel top-left. */
  x: number;
  y: number;
  /** The tile centre currently being walked to. Hunters move tile to tile and
   *  only ever re-decide on arrival, so their pixel position is derived rather
   *  than steered: a float position tested against a tile boundary is a test
   *  that never fires. */
  tx: number;
  ty: number;
  /** Last tile step taken, so a hunter does not turn back on itself. */
  dx: number;
  dy: number;
}

/**
 * Three compiler keys and a locked gate, on a clock, with hunters walking the
 * corridors. The keys are the whole job: the gate does not open for anything
 * else, and the clock does not care how close you were.
 */
function createCodeMaze(seed: number, level: number): StageRun {
  const L = MAZE_LEVELS[lvl(level)];
  const KEYS_NEEDED = L.keys;
  const MAZE_TIME = L.time;
  const rand = makeRandom(seed);
  const solid = (tx: number, ty: number): boolean =>
    tx < 0 || ty < 0 || tx >= COLS || ty >= ROWS || MAZE[ty][tx] === "#";

  // Spawn and gate come off the map; if the map ever says something impossible
  // (a gate behind a wall after an edit), the reachability pass below fixes it
  // rather than shipping a stage that cannot be finished.
  let spawn = { x: 1, y: 1 };
  let gate = { x: COLS - 2, y: 1 };
  for (let y = 0; y < ROWS; y++) {
    for (let x = 0; x < COLS; x++) {
      if (MAZE[y][x] === "P") spawn = { x, y };
      if (MAZE[y][x] === "X") gate = { x, y };
    }
  }

  // Flood fill from the spawn: the set of tiles the player can actually stand on.
  const reach: Cell[] = [];
  const seen = new Set<number>();
  const dist = new Map<number, number>();
  const idx = (x: number, y: number) => y * COLS + x;
  const queue: Cell[] = [spawn];
  seen.add(idx(spawn.x, spawn.y));
  dist.set(idx(spawn.x, spawn.y), 0);
  while (queue.length) {
    const c = queue.shift() as Cell;
    reach.push(c);
    const d = dist.get(idx(c.x, c.y)) ?? 0;
    for (const [dx, dy] of [
      [1, 0],
      [-1, 0],
      [0, 1],
      [0, -1],
    ]) {
      const nx = c.x + dx;
      const ny = c.y + dy;
      if (solid(nx, ny) || seen.has(idx(nx, ny))) continue;
      seen.add(idx(nx, ny));
      dist.set(idx(nx, ny), d + 1);
      queue.push({ x: nx, y: ny });
    }
  }

  const farthest = reach.reduce((a, b) =>
    (dist.get(idx(b.x, b.y)) ?? 0) > (dist.get(idx(a.x, a.y)) ?? 0) ? b : a,
  );
  if (!seen.has(idx(gate.x, gate.y))) gate = farthest;

  // Keys at three well-separated reachable tiles: farthest-point sampling off
  // the spawn and then off each other, so they are never bunched in one arm.
  const keys: Cell[] = [];
  const anchors: Cell[] = [spawn];
  const spread = (c: Cell) =>
    Math.min(...anchors.map((a) => Math.abs(a.x - c.x) + Math.abs(a.y - c.y)));
  const pool = reach.filter(
    (c) => !(c.x === spawn.x && c.y === spawn.y) && !(c.x === gate.x && c.y === gate.y),
  );
  for (let i = 0; i < KEYS_NEEDED && pool.length; i++) {
    const best = pool.reduce((a, b) => (spread(b) > spread(a) ? b : a));
    keys.push(best);
    anchors.push(best);
    pool.splice(pool.indexOf(best), 1);
  }
  const taken = keys.map(() => false);

  // Hunters start well away from the spawn so the first two seconds are yours.
  const hunters: Walker[] = [];
  const hunterPool = reach.filter((c) => (dist.get(idx(c.x, c.y)) ?? 0) > 8);
  // Two for the first two levels. A third only arrives at the last one, where
  // the corridors becoming a genuine gamble is the point.
  for (let i = 0; i < L.hunters && hunterPool.length; i++) {
    const pick = hunterPool[Math.floor(rand() * hunterPool.length)];
    hunterPool.splice(hunterPool.indexOf(pick), 1);
    hunters.push({
      x: pick.x * CELL + 2,
      y: pick.y * CELL + 2,
      tx: pick.x,
      ty: pick.y,
      dx: 0,
      dy: 0,
    });
  }

  let px = spawn.x * CELL + 2;
  let py = spawn.y * CELL + 2;
  let clock = MAZE_TIME;
  let status: RunStatus = "playing";
  let score = 0;
  let gateFlash = 0;
  const cues: Cue[] = [];

  /** Can a WALK-sized box sit at this pixel position without touching a wall? */
  function clear(x: number, y: number): boolean {
    const corners: [number, number][] = [
      [x, y],
      [x + WALK - 1, y],
      [x, y + WALK - 1],
      [x + WALK - 1, y + WALK - 1],
    ];
    return corners.every(([cx, cy]) => !solid(Math.floor(cx / CELL), Math.floor(cy / CELL)));
  }

  /** The pixel origin of a tile, for a WALK-sized box centred in it. */
  const originOf = (t: number) => t * CELL + Math.floor((CELL - WALK) / 2);

  /**
   * Axes resolved one at a time, with a little corner forgiveness.
   *
   * A corridor is 16px wide and the walker is 11, so a turn is only legal
   * inside a 5px window. Asking a player to find that window while something is
   * chasing them is how a maze stops being fun: when a turn is blocked purely
   * by being a few pixels off the corridor's line, nudge onto the line and take
   * the turn. Beyond that window the wall is a wall.
   */
  function move(x: number, y: number, dx: number, dy: number): [number, number] {
    const SLACK = Math.max(1, CELL - WALK);
    let nx = x;
    let ny = y;
    if (dx !== 0) {
      if (clear(x + dx, ny)) nx = x + dx;
      else {
        const ay = originOf(Math.round((ny - originOf(0)) / CELL));
        if (Math.abs(ay - ny) <= SLACK && clear(x + dx, ay)) {
          nx = x + dx;
          ny = ay;
        }
      }
    }
    if (dy !== 0) {
      if (clear(nx, ny + dy)) ny = ny + dy;
      else {
        const ax = originOf(Math.round((nx - originOf(0)) / CELL));
        if (Math.abs(ax - nx) <= SLACK && clear(ax, ny + dy)) {
          nx = ax;
          ny = ny + dy;
        }
      }
    }
    return [nx, ny];
  }

  const DIRS: [number, number][] = [
    [1, 0],
    [-1, 0],
    [0, 1],
    [0, -1],
  ];

  return {
    get status() {
      return status;
    },
    get score() {
      return score;
    },
    readout: () =>
      `KEYS ${taken.filter(Boolean).length}/${KEYS_NEEDED} · ${Math.ceil(clock)}s`,
    update(dt, input) {
      if (status !== "playing") return;
      if (gateFlash > 0) gateFlash = Math.max(0, gateFlash - dt);

      clock -= dt;
      if (clock <= 0) {
        status = "failed";
        cues.push("die");
        return;
      }

      const speed = 64 * dt;
      let dx = 0;
      let dy = 0;
      if (input.held.has("left")) dx -= speed;
      if (input.held.has("right")) dx += speed;
      if (input.held.has("up")) dy -= speed;
      if (input.held.has("down")) dy += speed;
      [px, py] = move(px, py, dx, dy);

      // Keys.
      keys.forEach((k, i) => {
        if (taken[i]) return;
        if (aabb(px, py, WALK, WALK, k.x * CELL + 4, k.y * CELL + 4, 8, 8)) {
          taken[i] = true;
          score += 60;
          cues.push("pickup");
          if (taken.every(Boolean)) {
            gateFlash = 0.9;
            cues.push("gate");
          }
        }
      });

      // Hunters walk tile to tile and only re-decide on arrival, which is what
      // keeps them readable enough to duck around: a corridor they entered is a
      // corridor they will finish.
      const pickTarget = (h: Walker): boolean => {
        const options = DIRS.filter(([ox, oy]) => !solid(h.tx + ox, h.ty + oy));
        const forward = options.filter(([ox, oy]) => !(ox === -h.dx && oy === -h.dy));
        const choices = forward.length ? forward : options;
        // A walled-in tile cannot happen in the shipped map, but a future edit
        // could make one, and a hunter with nowhere to go must park rather than
        // spin the arrival loop.
        if (!choices.length) return false;
        // Mostly wander, sometimes lean toward the player: a pure chase is
        // unfair on a board this size, and pure noise is furniture.
        const pick =
          rand() < L.hunt
            ? choices.reduce((a, b) => {
                const cost = ([ox, oy]: [number, number]) =>
                  Math.abs((h.tx + ox) * CELL - px) + Math.abs((h.ty + oy) * CELL - py);
                return cost(b) < cost(a) ? b : a;
              })
            : choices[Math.floor(rand() * choices.length)];
        h.dx = pick[0];
        h.dy = pick[1];
        h.tx += pick[0];
        h.ty += pick[1];
        return true;
      };

      for (const h of hunters) {
        let budget = L.speed * dt;
        // A loop rather than one step: at a high frame rate a hunter can cross
        // a tile boundary and start into the next one within a single tick, and
        // stopping dead at the boundary is a visible stutter.
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
        if (aabb(px, py, WALK, WALK, h.x, h.y, WALK, WALK)) {
          status = "failed";
          cues.push("die");
          return;
        }
      }

      // The gate.
      if (
        taken.every(Boolean) &&
        aabb(px, py, WALK, WALK, gate.x * CELL + 2, gate.y * CELL + 2, 12, 12)
      ) {
        status = "cleared";
        score += 150 + Math.round(clock) * 3;
        cues.push("clear");
      }
    },
    draw(g) {
      g.fillStyle = IN.ink;
      g.fillRect(0, 0, VIEW_W, VIEW_H);

      // Walls, drawn as circuit board traces rather than bricks.
      for (let y = 0; y < ROWS; y++) {
        for (let x = 0; x < COLS; x++) {
          if (!solid(x, y)) continue;
          const px0 = x * CELL;
          const py0 = y * CELL;
          g.fillStyle = IN.grid;
          g.fillRect(px0, py0, CELL, CELL);
          g.fillStyle = IN.dusk;
          g.fillRect(px0 + 1, py0 + 1, CELL - 2, 1);
          g.fillRect(px0 + 1, py0 + CELL - 2, CELL - 2, 1);
        }
      }

      // Gate: barred until the last key lands, then a lit doorway.
      const open = taken.every(Boolean);
      const gx = gate.x * CELL;
      const gy = gate.y * CELL;
      g.fillStyle = open ? (gateFlash > 0 ? IN.pale : IN.wire) : IN.wireDim;
      g.fillRect(gx + 2, gy + 2, 12, 12);
      g.fillStyle = IN.ink;
      if (open) {
        g.fillRect(gx + 5, gy + 5, 6, 6);
      } else {
        for (let i = 0; i < 3; i++) g.fillRect(gx + 4 + i * 4, gy + 3, 2, 10);
      }

      // Keys.
      keys.forEach((k, i) => {
        if (taken[i]) return;
        g.fillStyle = IN.gold;
        g.fillRect(k.x * CELL + 5, k.y * CELL + 4, 6, 4);
        g.fillRect(k.x * CELL + 7, k.y * CELL + 8, 2, 4);
        g.fillRect(k.x * CELL + 7, k.y * CELL + 10, 4, 2);
      });

      // Hunters.
      for (const h of hunters) {
        g.fillStyle = IN.hot;
        g.fillRect(Math.round(h.x), Math.round(h.y), WALK, WALK);
        g.fillStyle = IN.ink;
        g.fillRect(Math.round(h.x) + 2, Math.round(h.y) + 3, 2, 3);
        g.fillRect(Math.round(h.x) + 7, Math.round(h.y) + 3, 2, 3);
      }

      // The player: a cursor, because that is what you are.
      g.fillStyle = IN.pale;
      g.fillRect(Math.round(px) + 3, Math.round(py), 5, WALK);
      g.fillRect(Math.round(px), Math.round(py), WALK, 2);
      g.fillRect(Math.round(px), Math.round(py) + WALK - 2, WALK, 2);

      // Clock bar along the top: a number you have to read is a number you do
      // not read while something is chasing you.
      const frac = Math.max(0, clock / MAZE_TIME);
      g.fillStyle = frac < 0.25 ? IN.hot : IN.wire;
      g.fillRect(0, 0, Math.round(VIEW_W * frac), 2);
    },
    drain() {
      return cues.splice(0, cues.length);
    },
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
      return `${L.target} nodes, ${L.severed} severed circuits, and a thread that quickens with every one you take.`;
    },
    create: createKnotline,
  },
  {
    id: "bug-blaster",
    title: "BUG BLASTER",
    brief:
      "Mites are walking down the stack. Clear every wave. Your gun improves as you go, and the mites drop the rest.",
    controls: "LEFT / RIGHT to move · SPACE to fire (hold it) · X for a bomb",
    levelNote: (n) => {
      const L = BLAST_LEVELS[lvl(n)];
      return `${L.waves.length} waves, up to ${Math.max(...L.waves)} columns and ${L.rows} rows deep. You start on ${TIERS[L.startTier].name}.`;
    },
    create: createBugBlaster,
  },
  {
    id: "code-maze",
    title: "CODE MAZE",
    brief: "Collect every compiler key, then reach the gate. Mind the hunters, mind the clock.",
    controls: "ARROWS or WASD to move",
    levelNote: (n) => {
      const L = MAZE_LEVELS[lvl(n)];
      return `${L.keys} keys, ${L.time} seconds, ${L.hunters} hunters in the corridors.`;
    },
    create: createCodeMaze,
  },
];
