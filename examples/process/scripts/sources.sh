#!/bin/sh
mkdir -p app
printf "order: [U1, U2]\nunits:\n  - { id: U1, scope: [app/a/**], note: first }\n  - { id: U2, scope: [app/b/**], note: second }\n" > plan.yaml
echo "{\"met\": true, \"rationale\": \"fetched; 2 units planned\"}"
