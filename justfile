dev:
    cargo run

check:
    nd check

test:
    nd test

test-real:
    nd test --tier real

diagnose:
    nd diagnose --last

web:
    nd serve --target web
