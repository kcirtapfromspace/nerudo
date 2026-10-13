import type { Metadata } from "next";
import { Window } from "../../../src/components/Window";
import { BrandKit } from "../../../src/views/BrandKit";

export const metadata: Metadata = { title: "Brand & interface" };

export default function BrandPage() {
  return <Window title="Brand & interface" wide><BrandKit /></Window>;
}
