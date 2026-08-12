// One tribute page per stage cleared. Beating a game buys you a page of names.
//
// These are people, not products. Every entry is a person's name and a plain
// description of what they did, and no entry names a title, a console, a studio
// or a brand: partly because this repository does not put trademarks in its
// copy, and partly because it reads better. What these people actually did is
// more interesting than what it was called.
//
// Everything here is a matter of public record. If a line ever turns out to be
// wrong, fix the line: an inaccurate tribute is worse than no tribute.

export interface TributeEntry {
  name: string;
  /** What they did, in one line, in plain language. */
  note: string;
}

export interface Tribute {
  /** Small line above the heading. */
  kicker: string;
  heading: string;
  /** One or two sentences setting up the roll. */
  lede: string;
  roll: readonly TributeEntry[];
  /** The closing line under the roll. */
  close: string;
}

export const TRIBUTES: readonly Tribute[] = [
  {
    kicker: "STAGE ONE CLEARED",
    heading: "THE TITANS OF CODE",
    lede:
      "Before any of this was a game, it was an argument that a machine could be told what to do, by people who mostly had to build the machine as well.",
    roll: [
      { name: "Ada Lovelace", note: "saw that a calculating engine could work on anything you could symbolise, not just numbers" },
      { name: "Alan Turing", note: "described what computation is, and what no computer will ever do" },
      { name: "Grace Hopper", note: "argued that programs should be written in something a person could read, then built the compiler to prove it" },
      { name: "The ENIAC operators", note: "six women who programmed the first general-purpose electronic computer by hand, and were left out of the photographs" },
      { name: "Margaret Hamilton", note: "led the flight software that caught its own overload and landed anyway" },
      { name: "Frances Allen", note: "made compilers optimise, and made that a field" },
      { name: "Edsger Dijkstra", note: "gave us shortest paths, and would not let the profession be sloppy about correctness" },
      { name: "Barbara Liskov", note: "worked out what it means for one type to stand in for another" },
      { name: "Donald Knuth", note: "wrote it all down properly, and then wrote the typesetting system to print it" },
      { name: "Dennis Ritchie and Ken Thompson", note: "built the language and the operating system nearly everything since has been written on top of" },
      { name: "Karen Sparck Jones", note: "worked out how to weigh a word against every other word, which is why search works" },
      { name: "Radia Perlman", note: "kept networks from eating themselves, which is why any of this reaches you" },
    ],
    close: "Two more stages. Keep going.",
  },
  {
    kicker: "STAGE TWO CLEARED",
    heading: "THE TITANS OF PLAY",
    lede:
      "Then somebody asked what a computer was actually for, and answered: fun. Every one of these people was told at some point that it was not serious work.",
    roll: [
      { name: "Ralph Baer", note: "put the first game on an ordinary television set, and had to argue that anyone would want one" },
      { name: "Steve Russell", note: "wrote one of the first computer games on a machine the size of a room, and gave the source away" },
      { name: "Toru Iwatani", note: "designed a maze chase deliberately for people who did not think of themselves as players" },
      { name: "Carol Shaw", note: "the first woman to design and program a commercially released video game" },
      { name: "Dona Bailey", note: "co-designed an arcade hit aimed squarely at the players nobody else was designing for" },
      { name: "Roberta Williams", note: "made the adventure game something you could look at, not only read" },
      { name: "Danielle Bunten Berry", note: "built games about people in a room together, and insisted that was the point" },
      { name: "Shigeru Miyamoto", note: "designed play as a place to explore rather than a score to beat" },
      { name: "Rieko Kodama", note: "artist and director, among the first women to lead a console role-playing game" },
      { name: "Jordan Mechner", note: "filmed his brother running so that a handful of pixels could move like a person" },
      { name: "Yuji Naka", note: "wrote an engine fast enough that speed itself became the idea" },
      { name: "John Carmack", note: "made three dimensions run on machines with no business running them" },
      { name: "Every uncredited programmer", note: "who shipped a cabinet or a cartridge with their name nowhere on the box" },
    ],
    close: "One stage left.",
  },
  {
    kicker: "STAGE THREE CLEARED",
    heading: "THE ONES WHO HID THINGS",
    lede:
      "And somebody signed their work when they were told they could not. This whole tradition starts with a programmer who was refused a credit and put his name in the game anyway, in a room you could only reach by carrying an invisible object to a wall that should not have let you through.",
    roll: [
      {
        name: "Warren Robinett",
        note: "hid his name in a secret room in 1979 because programmers were not permitted to sign their own work. The publisher considered removing it, decided the cost was not worth it, and by then the idea had a name",
      },
      {
        name: "The ones who followed",
        note: "every developer since who has tucked a room, a joke, a portrait, a phone number or a thank-you somewhere only the curious would ever look",
      },
      {
        name: "The ones who went looking",
        note: "players who mapped walls that should have been solid, held buttons nobody told them to hold, and wrote it all down for strangers" ,
      },
    ],
    close: "You went looking. You found it. You finished it.",
  },
];
