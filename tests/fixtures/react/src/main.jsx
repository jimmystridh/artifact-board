import React from 'react';
import { createRoot } from 'react-dom/client';
function App() { const [count, setCount] = React.useState(0); return <main><h1>Artifact Board React fixture</h1><p>Built from a complete React source project.</p><button onClick={() => setCount(count + 1)}>Count: {count}</button></main>; }
createRoot(document.getElementById('root')).render(<App />);
