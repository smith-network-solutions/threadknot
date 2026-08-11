// The cabinet's mouth.
//
// House style: the machine is a smug old friend, not a bully. It teases the
// attempt, never the person. Nothing here comments on who you are, how clever
// you are, or how you compare to anyone else, because a joke a stranger reads
// at two in the morning after a bad day should still land as friendly. If a
// line would sting coming from a colleague, it does not belong in the list.
//
// Also: no brands, no titles, no other machines. The humour is about you and
// this cabinet.

/** After a life goes, with the run still alive. Light, quick, keep moving. */
export const LIFE_LOST: readonly string[] = [
  "The thread had one job.",
  "That was educational. For us.",
  "Bold strategy. Mixed execution.",
  "Somewhere a compiler just felt a warm glow.",
  "You were doing so well. Relatively.",
  "The machine would like the record to show that it warned you.",
  "Have you tried pressing the correct keys? Just as an experiment.",
  "That one goes in the highlight reel. The other highlight reel.",
  "Ten out of ten for commitment. Rather fewer for outcome.",
  "The cabinet is not angry. The cabinet is disappointed.",
  "You blinked. The machine does not have eyelids.",
  "Statistically speaking, that was avoidable.",
  "Approaching the problem sideways, I see.",
  "Not the plan, but undeniably a plan.",
  "The pixels are fine, thank you for asking.",
  "You had exactly one fewer life than you were playing with.",
  "That was a choice. It was made. We move on.",
  "Rewind is not a feature here. Regret is.",
  "Every expert was once a beginner. You are being very thorough about it.",
  "The good news is it cannot get worse. The news is not always good.",
  "Confidence: unaffected. Score: affected.",
  "A moment of silence for that perfectly reasonable idea.",
  "The board would like to thank you for your contribution.",
  "Somewhere, a rubber duck is shaking its head.",
];

/** Out of lives. Bigger, funnier, and it points at the door. */
export const GAME_OVER: readonly string[] = [
  "The machine remains undefeated. It has nowhere else to be.",
  "Three lives, gone like a long weekend.",
  "You fought the board. The board is still here.",
  "The high score table remains entirely unbothered.",
  "Nine levels exist. You were introduced to some of them.",
  "Every legend has a training montage. This was one frame of it.",
  "The cabinet thanks you for your generous donation of dignity.",
  "In another timeline you nailed that. This is not that timeline.",
  "Somewhere in a dim arcade, a machine just nodded knowingly.",
  "That is a wrap. The credits you did not reach send their regards.",
  "You have been defeated by rectangles. Sleep on that.",
  "The thread is fine. The thread is always fine. It is you we worry about.",
  "Take a lap. Come back with better reflexes and worse judgement.",
  "The machine has logged this. The machine logs everything.",
  "Not every run is a victory. This one was especially not.",
  "Excellent effort, technically speaking, in the loosest possible sense.",
  "You will get it. The question was never if. It was how many attempts.",
];

/** Shown while the cabinet is cooling off. Sympathetic, still smug. */
export const COOLDOWN: readonly string[] = [
  "Five credits, all gone. The machine has standards, even if its graphics do not.",
  "The tube needs to cool. Frankly, so do you.",
  "Scarcity is what made these things special. Also queues. Mostly queues.",
  "Go and do something else. The cabinet will still be smug when you return.",
  "This is the part where you would have run out of coins anyway.",
  "Absence makes the reflexes fonder.",
  "The machine is not ignoring you. It is pacing itself.",
  "Come back sharper. Or come back the same. It will be here either way.",
];

/**
 * Pick a line, avoiding the one just shown so a short losing streak does not
 * repeat itself. `previous` is whatever this picker returned last time.
 */
export function pickTaunt(list: readonly string[], previous?: string): string {
  if (list.length === 0) return "";
  if (list.length === 1) return list[0];
  const pool = previous ? list.filter((t) => t !== previous) : list;
  return pool[Math.floor(Math.random() * pool.length)] ?? list[0];
}
