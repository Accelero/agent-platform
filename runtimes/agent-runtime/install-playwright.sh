#!/bin/sh
set -eu

# Install the matching browser and its OS dependencies while building the image.
mkdir -p /opt/playwright
npm install --prefix /opt/playwright --save-exact "@playwright/test@${PLAYWRIGHT_VERSION:?}"
/opt/playwright/node_modules/.bin/playwright install --with-deps --only-shell chromium
ln -s /opt/playwright/node_modules/.bin/playwright /usr/local/bin/playwright
chmod -R a+rX /opt/playwright
rm -rf /var/lib/apt/lists/* /root/.npm
