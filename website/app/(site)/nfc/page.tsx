import type { Metadata } from "next";

import HomePage from "../page";

/* `/nfc` is the landing page that starts the song on arrival. SongDeepLink
   starts the song and puts the address back to `/`.

   Currently used for the OpenAPPA 3D-printed trinkets with NFC tags embedded,
   handed out at KubeCon Salt Lake City. We may want to use this /nfc route
   for something else in the future, but are reserving it now. */
export const metadata: Metadata = {
  alternates: { canonical: "/" },
  robots: { index: false },
};

export default HomePage;
