import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import './styles.css';
import { createTauriTransport } from './transport/client';

const host = document.getElementById('root');
if (host === null) {
  throw new Error('missing root element');
}

createRoot(host).render(
  <StrictMode>
    <App transport={createTauriTransport()} />
  </StrictMode>,
);
