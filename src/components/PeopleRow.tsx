import { useStore } from "../state/store";
import { OWNER_PERSON_ID, type Person } from "../lib/protocol";
import { MachineAvatar } from "./MachineAvatar";
import { UsersIcon } from "./icons";

/** One person's face. Reuses the machine badge: image when set, initials when
 *  not, ringed in their accent color. */
export function PersonAvatar({
  person,
  size = 22,
  preview = true,
}: {
  person: Person | undefined;
  size?: number;
  preview?: boolean;
}) {
  return (
    <MachineAvatar
      image={person?.avatar}
      color={person?.color}
      // A stamp whose record was deleted still renders as a face rather than
      // vanishing: the chat WAS somebody's, and quietly folding it into the
      // owner's would be a lie about who wrote it.
      name={person?.name ?? "Former teammate"}
      size={size}
      preview={preview}
    />
  );
}

/** The row of faces at the head of the sidebar: click one and the sidebar
 *  becomes that person's chats.
 *
 *  Faces at rest, a name only on the one you have selected. The sidebar is
 *  ~290px and a labelled chip is ~90px of it, so four teammates with their
 *  names showing scrolled off the edge — and a row you have to scroll defeats
 *  the point of being able to see at a glance who has something running.
 *
 *  Renders NOTHING on an install with one person on it, which is almost all of
 *  them. That is the whole backwards-compatibility contract on the UI side —
 *  until somebody adds a second person in Settings, the sidebar is exactly the
 *  sidebar it was.
 *
 *  Deliberately not a dropdown. Seeing at a glance that your intern has three
 *  chats running is most of the value of the feature, and a menu you have to
 *  open first hides precisely that. */
export function PeopleRow() {
  const { state, actions } = useStore();
  if (state.people.length < 2) return null;

  // Yours first, then everyone else by name — a stable order, so the face you
  // reach for does not move when somebody starts a chat.
  const ordered = [...state.people].sort((a, b) => {
    if (a.id === state.actingPerson) return -1;
    if (b.id === state.actingPerson) return 1;
    return a.name.localeCompare(b.name);
  });

  const counts = new Map<string, number>();
  for (const list of Object.values(state.threads)) {
    for (const thread of list) {
      const author = thread.author ?? OWNER_PERSON_ID;
      counts.set(author, (counts.get(author) ?? 0) + 1);
    }
  }

  const select = (personId: string | null) => {
    // Clicking the face you are already on goes back to everyone, so the row
    // is a toggle rather than a mode you have to find the exit from.
    actions.setViewPerson(state.viewPerson === personId ? null : personId);
  };

  return (
    <div className="sidebar-people" role="group" aria-label="Whose chats to show">
      <button
        type="button"
        className={`people-chip people-everyone${state.viewPerson === null ? " on" : ""}`}
        aria-pressed={state.viewPerson === null}
        aria-label="Everyone's chats"
        title="Everyone's chats"
        onClick={() => actions.setViewPerson(null)}
      >
        <span className="people-chip-face">
          <UsersIcon size={15} />
        </span>
        <span className="people-chip-name">Everyone</span>
      </button>
      {ordered.map((person) => {
        const active = state.viewPerson === person.id;
        const mine = person.id === state.actingPerson;
        const count = counts.get(person.id) ?? 0;
        const label = mine ? "You" : person.name;
        return (
          <button
            key={person.id}
            type="button"
            className={`people-chip${active ? " on" : ""}${mine ? " is-you" : ""}`}
            aria-pressed={active}
            aria-label={`${label} — ${count} ${count === 1 ? "chat" : "chats"}`}
            title={`${mine ? "Your" : `${person.name}'s`} chats (${count})`}
            onClick={() => select(person.id)}
          >
            <span className="people-chip-face">
              <PersonAvatar person={person} size={22} preview={false} />
              {count > 0 && <span className="people-chip-count">{count}</span>}
            </span>
            <span className="people-chip-name">{label}</span>
          </button>
        );
      })}
    </div>
  );
}
