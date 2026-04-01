"""Custom rule for building the SvelteKit frontend as a tree artifact.

Unlike genrule, this uses declare_directory() so Bazel tracks the
entire build output directory as a first-class artifact. Changes to
the output contents invalidate downstream action caches (e.g. the
server crate that embeds these files via rust-embed).
"""

def _frontend_build_impl(ctx):
    out_dir = ctx.actions.declare_directory("build")

    # Collect all source file paths for the command
    src_paths = [f.path for f in ctx.files.srcs]

    ctx.actions.run_shell(
        outputs = [out_dir],
        inputs = ctx.files.srcs,
        command = """
            set -e
            OUT_ABS="$PWD/{out}"
            FRONTEND=$(cd $(dirname {package_json}) && pwd -P)
            cd "$FRONTEND"
            if [ ! -d node_modules ]; then
                npm ci --silent 2>/dev/null
            fi
            rm -rf build
            npm run build 2>&1
            # Move build contents into the declared tree artifact
            mkdir -p "$OUT_ABS"
            mv build/* "$OUT_ABS/"
            # Also symlink from the exec root path where rust-embed looks
            # (CARGO_MANIFEST_DIR resolves to exec_root/crates/xpressclaw-server,
            # so ../../frontend/build/ = exec_root/frontend/build/)
            ln -sfn "$OUT_ABS" "$FRONTEND/build"
        """.format(
            package_json = ctx.file.package_json.path,
            out = out_dir.path,
        ),
        use_default_shell_env = True,  # inherit PATH so npm/node are found
        execution_requirements = {"local": "1"},  # needs npm, node, filesystem
        mnemonic = "SvelteKitBuild",
        progress_message = "Building SvelteKit frontend",
    )

    return [DefaultInfo(files = depset([out_dir]))]

frontend_build = rule(
    implementation = _frontend_build_impl,
    attrs = {
        "srcs": attr.label_list(allow_files = True),
        "package_json": attr.label(allow_single_file = True),
    },
)
