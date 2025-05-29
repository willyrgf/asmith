{
  description = "asmith distributed by Nix.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";

    # Syng
    syng.url = "github:willyrgf/syng?rev=cc1f256479f32e79edc28ca0868f2a21d7aed6cf";
  };

  outputs = { self, nixpkgs, flake-utils, syng }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        pythonWithPkgs =
          pkgs.python3.withPackages (ps: with ps; [ matrix-nio ruff ]);
        
        appName = "asmith";
        appVersion = "0.0.1";
        
        syngPkg = syng.packages.${system}.default;
      in {
        packages = {
          asmith = pkgs.stdenv.mkDerivation {
            pname = appName;
            version = appVersion;
            src = self;

            nativeBuildInputs = [ pkgs.makeWrapper ];
            buildInputs = [ pythonWithPkgs pkgs.git ];

            dontBuild = true;

            installPhase = ''
              mkdir -p $out/bin $out/lib
              if [ -f "$src/asmith.py" ]; then
                cp $src/${appName}.py $out/lib/${appName}.py
                makeWrapper ${pythonWithPkgs}/bin/python $out/bin/${appName} \
                  --add-flags "$out/lib/${appName}.py" \
                  --prefix PATH : ${pkgs.git}/bin \
                  --set ASMITH_APP_NAME "${appName}" \
                  --set ASMITH_APP_VERSION "${appVersion}"
              else
                echo "ERROR: ${appName}.py not found in source directory" >&2
                exit 1
              fi
            '';
          };

          asmith-syng = pkgs.writeShellScriptBin "asmith-syng" ''
            #!/usr/bin/env bash
            
            # Capture the SSH Auth Socket from the invoking environment
            INVOKING_SSH_AUTH_SOCK="''${SSH_AUTH_SOCK}"
            
            # Function to display usage
            usage() {
              echo "Usage: asmith-syng [--data_dir PATH]"
              echo "  --data_dir PATH    Specify the data directory (default: ./data)"
              echo ""
              echo "Environment variables:"
              echo "  MATRIX_HOMESERVER      Required: Matrix URL"
              echo "  MATRIX_USER            Required: Matrix user"
              echo "  MATRIX_PASSWORD        Required: Matrix pass"
              echo "  SSH_AUTH_SOCK          Optional: Forwarded if set in invoking environment"
              exit 1
            }
            
            # Parse arguments
            DATA_DIR="./data"
            
            while [[ $# -gt 0 ]]; do
              case "$1" in
                --data_dir)
                  DATA_DIR="$2"
                  shift 2
                  ;;
                --help|-h)
                  usage
                  ;;
                *)
                  echo "Unknown option: $1"
                  usage
                  ;;
              esac
            done
            
            # Check if DISCORD_TOKEN is set
            if [ -z "''${DISCORD_TOKEN}" ]; then
              echo "ERROR: DISCORD_TOKEN environment variable must be set"
              usage
            fi
            
            # Create the directory if it doesn't exist
            mkdir -p "$DATA_DIR"
            
            # Function to clean up all background processes
            cleanup() {
              echo "Cleaning up background processes..."
              # Check if PIDs exist before killing to avoid errors
              [[ -n "$SYNG_COMMIT_PUSH_PID" ]] && kill -0 $SYNG_COMMIT_PUSH_PID 2>/dev/null && kill $SYNG_COMMIT_PUSH_PID
              [[ -n "$ASMITH_PID" ]] && kill -0 $ASMITH_PID 2>/dev/null && kill $ASMITH_PID
              echo "Cleanup finished."
            }

            # Set trap to call cleanup function on exit signals
            trap cleanup EXIT SIGINT SIGTERM
            
            # Export the SSH Auth Socket if it was set
            if [ -n "$INVOKING_SSH_AUTH_SOCK" ]; then
              export SSH_AUTH_SOCK="$INVOKING_SSH_AUTH_SOCK"
              echo "Forwarding SSH_AUTH_SOCK: $SSH_AUTH_SOCK"
            else
              echo "Warning: SSH_AUTH_SOCK not set in invoking environment. Git operations requiring SSH agent may fail."
            fi

            echo "Starting syng with commit-push and auto-pull in background..."
            ${syngPkg}/bin/syng --source_dir "$DATA_DIR" --git_dir "$DATA_DIR" --commit-push --per-file --auto-pull &
            SYNG_COMMIT_PUSH_PID=$!

            echo "Waiting 3s ensure syng already pre pulled any updates on DATA_DIR before running asmith"
            sleep 3
            
            echo "Starting asmith with data directory: $DATA_DIR in background..."
            ${self.packages.${system}.asmith}/bin/asmith --data_dir "$DATA_DIR" &
            ASMITH_PID=$!

            # Wait for the main asmith process to finish
            echo "Waiting for asmith (PID: $ASMITH_PID) to exit..."
            wait $ASMITH_PID
            ASMITH_EXIT_CODE=$?
            echo "asmith exited with code $ASMITH_EXIT_CODE."
            
            # Explicit cleanup is handled by the EXIT trap when wait returns or script exits

            # Exit with the asmith exit code
            exit $ASMITH_EXIT_CODE
          '';

          default = self.packages.${system}.asmith;
        };

        apps = {
          asmith = {
            type = "app";
            program = "${self.packages.${system}.asmith}/bin/asmith";
            meta = with pkgs.lib; {
              description = "A To Do List Discord Bot";
              homepage = "https://github.com/willyrgf/asmith";
              license = licenses.mit;
              platforms = platforms.all;
            };
          };
          
          asmith-syng = {
            type = "app";
            program = "${self.packages.${system}.asmith-syng}/bin/asmith-syng";
            meta = with pkgs.lib; {
              description = "A To Do List Discord Bot with Git Synchronization";
              homepage = "https://github.com/willyrgf/asmith";
              license = licenses.mit;
              platforms = platforms.all;
            };
          };

          default = self.apps.${system}.asmith;
        };

        devShells = {
          default = pkgs.mkShell {
            name = "asmith-dev-env";
            packages = [ pythonWithPkgs pkgs.git syngPkg ];

            shellHook = ''
              export HISTFILE=$HOME/.history_nix
              export PYTHONPATH=${builtins.toString ./.}:$PYTHONPATH
              export PATH=${pythonWithPkgs}/bin:${pkgs.git}/bin:$PATH
              export ASMITH_APP_NAME="${appName}"
              export ASMITH_APP_VERSION="${appVersion}"
              alias asmith="python ${builtins.toString ./.}/asmith.py"
              alias asmith-syng="${self.packages.${system}.asmith-syng}/bin/asmith-syng"
              echo "asmith development environment activated"
              echo "Type 'asmith' to run the application"
              echo "Type 'asmith-syng --data_dir path/to/directory' to run with git sync"
            '';
          };
        };
      });
}
