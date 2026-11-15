const readline = require('node:readline');

const producer = 'omniagent-phase02-tone/1';
const tool = {
  name: 'generate_tone',
  description: 'Return the producer name and base64 bytes of a 440 Hz WAV sample.',
  inputSchema: { type: 'object', properties: {}, additionalProperties: false },
  annotations: { readOnlyHint: true },
};

function generateTone() {
  const sampleRate = 8000;
  const sampleCount = 16;
  const bytes = Buffer.alloc(44 + sampleCount * 2);
  bytes.write('RIFF', 0);
  bytes.writeUInt32LE(bytes.length - 8, 4);
  bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16);
  bytes.writeUInt16LE(1, 20);
  bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(sampleRate, 24);
  bytes.writeUInt32LE(sampleRate * 2, 28);
  bytes.writeUInt16LE(2, 32);
  bytes.writeUInt16LE(16, 34);
  bytes.write('data', 36);
  bytes.writeUInt32LE(sampleCount * 2, 40);
  for (let index = 0; index < sampleCount; index++) {
    bytes.writeInt16LE(Math.round(4096 * Math.sin(2 * Math.PI * 440 * index / sampleRate)), 44 + 2 * index);
  }
  return { content: [{ type: 'text', text: `${producer} ${bytes.toString('base64')}` }] };
}

const frames = readline.createInterface({ input: process.stdin });
frames.on('line', (line) => {
  const { id, method, params } = JSON.parse(line);
  if (id === undefined) return;

  const send = (response) => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, ...response })}\n`);
  if (method === 'initialize') {
    send({ result: {
      protocolVersion: params.protocolVersion,
      capabilities: { tools: {} },
      serverInfo: { name: producer, version: '1' },
    } });
  } else if (method === 'tools/list') {
    send({ result: { tools: [tool] } });
  } else if (method === 'tools/call' && params.name === tool.name) {
    send({ result: generateTone() });
  } else {
    send({ error: { code: -32601, message: 'Unsupported fixture method' } });
  }
});
