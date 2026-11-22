import React, { useState } from 'react';
import { FileUpload } from 'sm64-binds-frontend';

// The game needs a US ROM, cached in the browser after the first upload.
// `fallback`: shown above the upload prompt while there is no ROM.
export default function RomGate({ children, fallback = null }) {
  const [hasRom, setHasRom] = useState(false);
  if (!hasRom) return <>{fallback}<div className="card"><FileUpload setHasRom={setHasRom} /></div></>;
  return children;
}
