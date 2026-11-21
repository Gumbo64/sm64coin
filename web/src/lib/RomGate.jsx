import React, { useState } from 'react';
import { FileUpload } from 'sm64-binds-frontend';

// The game needs a US ROM, cached in the browser after the first upload.
export default function RomGate({ children }) {
  const [hasRom, setHasRom] = useState(false);
  if (!hasRom) return <div className="card"><FileUpload setHasRom={setHasRom} /></div>;
  return children;
}
