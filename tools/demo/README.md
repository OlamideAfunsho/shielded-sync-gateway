How the videos in docs/media were made. Nothing in them is mocked. The
terminal panes replay real script runs line by line, with long waits
shortened (an on-screen clock shows real elapsed time). Every number on the
summary slides is read from `results/`.

    # step 1 video
    python3 tools/demo/record.py run.jsonl -- scripts/step1.sh
    python3 tools/demo/render.py run.jsonl results/step1 step1-demo.mp4

    # MVP video (steps 2-4); run step3.sh and bench.sh first
    python3 tools/demo/record.py step4-run.jsonl -- scripts/step4.sh
    cd tools/demo && python3 render_mvp.py ../../step4-run.jsonl ../../results ../../docs/media/mvp-demo.mp4

Needs Pillow, ffmpeg, the Inter font and DejaVu Sans Mono.
