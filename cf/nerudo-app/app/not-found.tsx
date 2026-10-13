import { Window } from "../src/components/Window";
import { Availability } from "../src/components/Availability";

export default function NotFound() {
  return (
    <Window title="Not found">
      <Availability title="This page is unavailable." message="This repository or organization is unavailable to you. Open your sign-in, share, or personal link to access a private repository." />
    </Window>
  );
}
