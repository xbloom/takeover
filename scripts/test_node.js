const http = require('http');
const req = http.get('https://httpbin.org/ip', (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => {
        try {
            const result = JSON.parse(data);
            console.log('Node.js HTTP result:', result);
        } catch (e) {
            console.log('Node.js parsing error:', e.message);
        }
    });
});

req.on('error', (err) => {
    console.log('Node.js HTTP error:', err.message);
});

req.setTimeout(10000, () => {
    console.log('Node.js HTTP timeout');
    req.destroy();
});

const req2 = http.get('https://ipinfo.io/ip', (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => {
        try {
            const result = JSON.parse(data);
            console.log('Node.js HTTP result:', result);
        } catch (e) {
            console.log('Node.js parsing error:', e.message);
        }
    });
});

req2.on('error', (err) => {
    console.log('Node.js HTTP error:', err.message);
});

req2.setTimeout(10000, () => {
    console.log('Node.js HTTP timeout');
    req.destroy();
});