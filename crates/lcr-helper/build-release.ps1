param(
    [switch]$SignedBroker
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if (-not $IsWindows) {
    throw 'LCR_HELPER_RELEASE_BUILD_REQUIRES_WINDOWS'
}

$RepoRoot = [IO.Path]::GetFullPath(
    (Join-Path $PSScriptRoot '..\..')
)

Set-Location -LiteralPath $RepoRoot

$EmbedEnv = 'LCR_EMBEDDED_BROKER_PATH'
$ManifestEmbedEnv = 'LCR_EMBEDDED_BROKER_MANIFEST_PATH'

$ResourceId = 101
$ManifestResourceId = 102

$SigningKey = Join-Path `
    $HOME `
    '.lcr-release\broker-signing-ed25519-private.pem'

$PinnedPublicKey = Join-Path `
    $RepoRoot `
    'crates\lcr-helper\assets\broker-signing-public-key.bin'

$SignedTemporaryPaths =
    [System.Collections.Generic.List[string]]::new()

$OldEmbed = [Environment]::GetEnvironmentVariable(
    $EmbedEnv,
    [EnvironmentVariableTarget]::Process
)

$OldManifestEmbed = [Environment]::GetEnvironmentVariable(
    $ManifestEmbedEnv,
    [EnvironmentVariableTarget]::Process
)

function Invoke-Cargo {
    param(
        [string]$Name,
        [string[]]$Arguments
    )

    & cargo @Arguments

    if ($LASTEXITCODE -ne 0) {
        throw "$Name EXIT=$LASTEXITCODE"
    }
}
function Test-ByteArrayEqual {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Left,

        [Parameter(Mandatory)]
        [byte[]]$Right
    )

    if ($Left.Length -ne $Right.Length) {
        return $false
    }

    for ($i = 0; $i -lt $Left.Length; $i++) {
        if ($Left[$i] -ne $Right[$i]) {
            return $false
        }
    }

    return $true
}

function Resolve-Ed25519OpenSsl {
    $Candidates =
        [System.Collections.Generic.List[string]]::new()

    $PathOpenSsl =
        Get-Command openssl.exe -ErrorAction SilentlyContinue

    if ($null -ne $PathOpenSsl) {
        $Candidates.Add($PathOpenSsl.Source)
    }

    $Git =
        Get-Command git.exe -ErrorAction SilentlyContinue

    if ($null -ne $Git) {
        $GitExe = [IO.Path]::GetFullPath($Git.Source)
        $GitRoot = Split-Path -Parent (Split-Path -Parent $GitExe)

        $Candidates.Add(
            (Join-Path $GitRoot 'mingw64\bin\openssl.exe')
        )

        $Candidates.Add(
            (Join-Path $GitRoot 'usr\bin\openssl.exe')
        )
    }

    $Candidates.Add(
        'C:\Program Files\Git\mingw64\bin\openssl.exe'
    )

    foreach ($Candidate in ($Candidates | Select-Object -Unique)) {
        if (-not (
            Test-Path `
                -LiteralPath $Candidate `
                -PathType Leaf
        )) {
            continue
        }

        & $Candidate version *> $null

        if ($LASTEXITCODE -ne 0) {
            continue
        }

        $Algorithms = @(
            & $Candidate `
                list `
                -public-key-algorithms `
                2>&1
        )

        if ($LASTEXITCODE -ne 0) {
            continue
        }

        $HasEd25519 = @(
            $Algorithms |
                Where-Object {
                    $_.ToString().IndexOf(
                        'ED25519',
                        [StringComparison]::OrdinalIgnoreCase
                    ) -ge 0
                }
        ).Count -gt 0

        if ($HasEd25519) {
            return [IO.Path]::GetFullPath($Candidate)
        }
    }

    throw 'SIGNED_BROKER_ED25519_OPENSSL_NOT_FOUND'
}

function Assert-SigningKeyAcl {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    if (-not (
        Test-Path `
            -LiteralPath $Path `
            -PathType Leaf
    )) {
        throw 'SIGNED_BROKER_PRIVATE_KEY_NOT_FOUND'
    }

    $Item = Get-Item -LiteralPath $Path

    if ($Item.Length -le 0) {
        throw 'SIGNED_BROKER_PRIVATE_KEY_EMPTY'
    }

    $CurrentSid =
        [Security.Principal.WindowsIdentity]::GetCurrent().User

    if ($null -eq $CurrentSid) {
        throw 'CURRENT_USER_SID_UNAVAILABLE'
    }

    $SystemSid =
        [Security.Principal.SecurityIdentifier]::new(
            [Security.Principal.WellKnownSidType]::LocalSystemSid,
            $null
        )

    $Allowed = @(
        $CurrentSid.Value,
        $SystemSid.Value
    )

    $Acl = Get-Acl -LiteralPath $Path

    if (-not $Acl.AreAccessRulesProtected) {
        throw 'SIGNED_BROKER_PRIVATE_KEY_ACL_INHERITS'
    }

    foreach ($Rule in $Acl.Access) {
        $Sid = $Rule.IdentityReference.Translate(
            [Security.Principal.SecurityIdentifier]
        ).Value

        if ($Rule.AccessControlType -ne
            [Security.AccessControl.AccessControlType]::Allow) {
            throw 'SIGNED_BROKER_PRIVATE_KEY_DENY_RULE_UNEXPECTED'
        }

        if ($Sid -notin $Allowed) {
            throw 'SIGNED_BROKER_PRIVATE_KEY_ACL_TOO_BROAD'
        }
    }
}

try {
    Remove-Item `
        -LiteralPath "Env:$EmbedEnv" `
        -ErrorAction SilentlyContinue

    Remove-Item `
        -LiteralPath "Env:$ManifestEmbedEnv" `
        -ErrorAction SilentlyContinue

    if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
        $TargetRoot = Join-Path $RepoRoot 'target'
    }
    else {
        $TargetRoot = [IO.Path]::GetFullPath(
            $env:CARGO_TARGET_DIR
        )
    }

    $ReleaseDir = Join-Path $TargetRoot 'release'

    $BrokerArgs = @(
        'build',
        '--release',
        '--locked',
        '-p', 'lcr-helper',
        '--bin', 'LCRInputBroker'
    )

    Invoke-Cargo `
        -Name 'BUILD_BROKER' `
        -Arguments $BrokerArgs

    $BrokerPath = (
        Resolve-Path `
            -LiteralPath (Join-Path $ReleaseDir 'LCRInputBroker.exe')
    ).Path

    $BrokerItem = Get-Item -LiteralPath $BrokerPath

    if ($BrokerItem.Length -le 0) {
        throw 'BROKER_BINARY_EMPTY'
    }

    $BrokerHash = (
        Get-FileHash `
            -LiteralPath $BrokerPath `
            -Algorithm SHA256
    ).Hash.ToLowerInvariant()

    if ($SignedBroker) {
        #
        # Signed release mode is explicit and never used by ordinary CI/dev
        # builds. The private key stays outside the repository and is consumed
        # only by OpenSSL; PowerShell never reads its key bytes.
        #

        $RepoPrefix =
            [IO.Path]::GetFullPath($RepoRoot).TrimEnd('\') + '\'

        $SigningKeyFull =
            [IO.Path]::GetFullPath($SigningKey)

        if ($SigningKeyFull.StartsWith(
            $RepoPrefix,
            [StringComparison]::OrdinalIgnoreCase
        )) {
            throw 'SIGNED_BROKER_PRIVATE_KEY_INSIDE_REPOSITORY'
        }

        Assert-SigningKeyAcl -Path $SigningKey

        [byte[]]$PinnedRaw =
            [IO.File]::ReadAllBytes($PinnedPublicKey)

        if ($PinnedRaw.Length -ne 32) {
            throw "PINNED_BROKER_PUBLIC_KEY_BYTES=$($PinnedRaw.Length)"
        }

        if (@(
            $PinnedRaw |
                Where-Object { $_ -ne 0 }
        ).Count -eq 0) {
            throw 'PINNED_BROKER_PUBLIC_KEY_ALL_ZERO'
        }

        $OpenSsl = Resolve-Ed25519OpenSsl

        $MetadataJson = (
            & cargo `
                metadata `
                --locked `
                --no-deps `
                --format-version 1
        ) | Out-String

        if ($LASTEXITCODE -ne 0) {
            throw 'SIGNED_BROKER_CARGO_METADATA_FAILED'
        }

        $Metadata = $MetadataJson | ConvertFrom-Json

        $Package = @(
            $Metadata.packages |
                Where-Object {
                    $_.name -eq 'lcr-helper'
                }
        )

        if ($Package.Count -ne 1) {
            throw "SIGNED_BROKER_PACKAGE_COUNT=$($Package.Count)"
        }

        $CanonicalVersion = $Package[0].version

        if ([string]::IsNullOrWhiteSpace($CanonicalVersion)) {
            throw 'SIGNED_BROKER_VERSION_EMPTY'
        }

        [byte[]]$VersionBytes =
            [Text.Encoding]::UTF8.GetBytes($CanonicalVersion)

        if ($VersionBytes.Length -eq 0 -or
            $VersionBytes.Length -gt 128) {
            throw "SIGNED_BROKER_VERSION_BYTES=$($VersionBytes.Length)"
        }

        $BrokerVersionBefore = (
            [Diagnostics.FileVersionInfo]::GetVersionInfo(
                $BrokerPath
            )
        ).ProductVersion

        if ($BrokerVersionBefore -cne $CanonicalVersion) {
            throw (
                'SIGNED_BROKER_PRODUCT_VERSION_MISMATCH ' +
                "expected=$CanonicalVersion " +
                "actual=$BrokerVersionBefore"
            )
        }

        $SignedMessagePath =
            Join-Path $ReleaseDir '.lcr-broker-sign-message.bin'

        $SignaturePath =
            Join-Path $ReleaseDir '.lcr-broker-signature.bin'

        $ManifestPath =
            Join-Path $ReleaseDir '.lcr-broker-manifest.bin'

        $PinnedPublicDerPath =
            Join-Path $ReleaseDir '.lcr-broker-pinned-public.der'

        $DerivedPublicDerPath =
            Join-Path $ReleaseDir '.lcr-broker-derived-public.der'

        $EmbeddedMessagePath =
            Join-Path $ReleaseDir '.lcr-broker-embedded-message.bin'

        $EmbeddedSignaturePath =
            Join-Path $ReleaseDir '.lcr-broker-embedded-signature.bin'

        foreach ($Temporary in @(
            $SignedMessagePath,
            $SignaturePath,
            $ManifestPath,
            $PinnedPublicDerPath,
            $DerivedPublicDerPath,
            $EmbeddedMessagePath,
            $EmbeddedSignaturePath
        )) {
            $SignedTemporaryPaths.Add($Temporary)

            Remove-Item `
                -LiteralPath $Temporary `
                -Force `
                -ErrorAction SilentlyContinue
        }

        #
        # Derive the public identity from the private key and require it to
        # match the committed pinned raw 32-byte public key exactly.
        #

        & $OpenSsl `
            pkey `
            -in $SigningKey `
            -pubout `
            -outform DER `
            -out $DerivedPublicDerPath `
            *> $null

        if ($LASTEXITCODE -ne 0) {
            throw 'SIGNED_BROKER_PUBLIC_DERIVATION_FAILED'
        }

        [byte[]]$DerivedDer =
            [IO.File]::ReadAllBytes($DerivedPublicDerPath)

        [byte[]]$SpkiPrefix = @(
            0x30, 0x2A,
            0x30, 0x05,
            0x06, 0x03,
            0x2B, 0x65, 0x70,
            0x03, 0x21, 0x00
        )

        if ($DerivedDer.Length -ne 44) {
            throw "SIGNED_BROKER_DERIVED_SPKI_BYTES=$($DerivedDer.Length)"
        }

        for ($i = 0; $i -lt $SpkiPrefix.Length; $i++) {
            if ($DerivedDer[$i] -ne $SpkiPrefix[$i]) {
                throw "SIGNED_BROKER_DERIVED_SPKI_PREFIX_AT=$i"
            }
        }

        [byte[]]$DerivedRaw = New-Object byte[] 32

        [Array]::Copy(
            $DerivedDer,
            12,
            $DerivedRaw,
            0,
            32
        )

        if (-not (
            Test-ByteArrayEqual `
                -Left $DerivedRaw `
                -Right $PinnedRaw
        )) {
            throw 'SIGNED_BROKER_PRIVATE_KEY_NOT_PINNED_IDENTITY'
        }

        #
        # Build a DER SubjectPublicKeyInfo using only the committed pinned
        # public key. Post-build verification never trusts a caller key.
        #

        [byte[]]$PinnedDer =
            New-Object byte[] ($SpkiPrefix.Length + $PinnedRaw.Length)

        [Array]::Copy(
            $SpkiPrefix,
            0,
            $PinnedDer,
            0,
            $SpkiPrefix.Length
        )

        [Array]::Copy(
            $PinnedRaw,
            0,
            $PinnedDer,
            $SpkiPrefix.Length,
            $PinnedRaw.Length
        )

        [IO.File]::WriteAllBytes(
            $PinnedPublicDerPath,
            $PinnedDer
        )

        [byte[]]$BrokerHashBytes =
            [Convert]::FromHexString($BrokerHash)

        if ($BrokerHashBytes.Length -ne 32) {
            throw 'SIGNED_BROKER_SHA256_SHAPE_INVALID'
        }

        #
        # LCRBSIG1:
        #   magic[8]
        #   format_version u16 LE
        #   version_length u16 LE
        #   payload_length u64 LE
        #   payload_sha256[32]
        #   canonical_semver[n]
        #   signature[64]
        #

        $Memory = [IO.MemoryStream]::new()

        $Writer = [IO.BinaryWriter]::new(
            $Memory,
            [Text.Encoding]::UTF8,
            $true
        )

        try {
            $Writer.Write(
                [Text.Encoding]::ASCII.GetBytes('LCRBSIG1')
            )

            $Writer.Write([UInt16]1)
            $Writer.Write([UInt16]$VersionBytes.Length)
            $Writer.Write([UInt64]$BrokerItem.Length)
            $Writer.Write($BrokerHashBytes)
            $Writer.Write($VersionBytes)
            $Writer.Flush()

            [byte[]]$SignedMessageBytes =
                $Memory.ToArray()
        }
        finally {
            $Writer.Dispose()
            $Memory.Dispose()
        }

        $ExpectedSignedMessageLength =
            52 + $VersionBytes.Length

        if ($SignedMessageBytes.Length -ne
            $ExpectedSignedMessageLength) {
            throw (
                'SIGNED_BROKER_MESSAGE_BYTES=' +
                $SignedMessageBytes.Length
            )
        }

        [IO.File]::WriteAllBytes(
            $SignedMessagePath,
            $SignedMessageBytes
        )

        & $OpenSsl `
            pkeyutl `
            -sign `
            -rawin `
            -inkey $SigningKey `
            -in $SignedMessagePath `
            -out $SignaturePath `
            *> $null

        if ($LASTEXITCODE -ne 0) {
            throw 'SIGNED_BROKER_ED25519_SIGN_FAILED'
        }

        [byte[]]$SignatureBytes =
            [IO.File]::ReadAllBytes($SignaturePath)

        if ($SignatureBytes.Length -ne 64) {
            throw "SIGNED_BROKER_SIGNATURE_BYTES=$($SignatureBytes.Length)"
        }

        #
        # Verify immediately against the committed pinned public key before
        # the manifest is eligible to enter LCR.exe.
        #

        & $OpenSsl `
            pkeyutl `
            -verify `
            -rawin `
            -pubin `
            -keyform DER `
            -inkey $PinnedPublicDerPath `
            -in $SignedMessagePath `
            -sigfile $SignaturePath `
            *> $null

        if ($LASTEXITCODE -ne 0) {
            throw 'SIGNED_BROKER_PREBUNDLE_SIGNATURE_VERIFY_FAILED'
        }

        [byte[]]$ManifestBytes =
            New-Object byte[] (
                $SignedMessageBytes.Length +
                $SignatureBytes.Length
            )

        [Array]::Copy(
            $SignedMessageBytes,
            0,
            $ManifestBytes,
            0,
            $SignedMessageBytes.Length
        )

        [Array]::Copy(
            $SignatureBytes,
            0,
            $ManifestBytes,
            $SignedMessageBytes.Length,
            $SignatureBytes.Length
        )

        [IO.File]::WriteAllBytes(
            $ManifestPath,
            $ManifestBytes
        )

        Set-Item `
            -LiteralPath "Env:$ManifestEmbedEnv" `
            -Value $ManifestPath
    }
    Set-Item `
        -LiteralPath "Env:$EmbedEnv" `
        -Value $BrokerPath

    $LcrArgs = @(
        'build',
        '--release',
        '--locked',
        '-p', 'lcr-helper',
        '--bin', 'LCR'
    )

    Invoke-Cargo `
        -Name 'BUILD_BUNDLED_LCR' `
        -Arguments $LcrArgs

    $LcrPath = (
        Resolve-Path `
            -LiteralPath (Join-Path $ReleaseDir 'LCR.exe')
    ).Path

    $ReaderSource = @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class LcrCanonicalBundleReaderP2B2A2
{
    private const uint LOAD_LIBRARY_AS_DATAFILE = 0x00000002;
    private const uint LOAD_LIBRARY_AS_IMAGE_RESOURCE = 0x00000020;
    private const int RT_RCDATA = 10;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryExW(
        string lpFileName,
        IntPtr hFile,
        uint dwFlags
    );

    [DllImport(
        "kernel32.dll",
        EntryPoint = "FindResourceW",
        SetLastError = true
    )]
    private static extern IntPtr FindResource(
        IntPtr hModule,
        IntPtr lpName,
        IntPtr lpType
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr LoadResource(
        IntPtr hModule,
        IntPtr hResInfo
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint SizeofResource(
        IntPtr hModule,
        IntPtr hResInfo
    );

    [DllImport("kernel32.dll")]
    private static extern IntPtr LockResource(
        IntPtr hResData
    );

    [DllImport("kernel32.dll")]
    private static extern bool FreeLibrary(
        IntPtr hModule
    );

    public static byte[] ReadRcData(
        string path,
        int resourceId
    )
    {
        IntPtr module = LoadLibraryExW(
            path,
            IntPtr.Zero,
            LOAD_LIBRARY_AS_DATAFILE |
                LOAD_LIBRARY_AS_IMAGE_RESOURCE
        );

        if (module == IntPtr.Zero)
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error()
            );
        }

        try
        {
            IntPtr resource = FindResource(
                module,
                new IntPtr(resourceId),
                new IntPtr(RT_RCDATA)
            );

            if (resource == IntPtr.Zero)
            {
                throw new InvalidOperationException(
                    "embedded broker RCDATA resource is missing"
                );
            }

            uint size = SizeofResource(
                module,
                resource
            );

            if (size == 0)
            {
                throw new InvalidOperationException(
                    "embedded broker RCDATA resource is empty"
                );
            }

            IntPtr loaded = LoadResource(
                module,
                resource
            );

            if (loaded == IntPtr.Zero)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error()
                );
            }

            IntPtr data = LockResource(loaded);

            if (data == IntPtr.Zero)
            {
                throw new InvalidOperationException(
                    "LockResource returned null"
                );
            }

            byte[] bytes = new byte[size];

            Marshal.Copy(
                data,
                bytes,
                0,
                checked((int)size)
            );

            return bytes;
        }
        finally
        {
            FreeLibrary(module);
        }
    }
}
"@

    if (-not ('LcrCanonicalBundleReaderP2B2A2' -as [type])) {
        Add-Type `
            -TypeDefinition $ReaderSource `
            -Language CSharp `
            -ErrorAction Stop
    }

    [byte[]]$EmbeddedBytes =
        [LcrCanonicalBundleReaderP2B2A2]::ReadRcData(
            $LcrPath,
            $ResourceId
        )

    [byte[]]$EmbeddedManifestBytes = $null

    if ($SignedBroker) {
        $EmbeddedManifestBytes =
            [LcrCanonicalBundleReaderP2B2A2]::ReadRcData(
                $LcrPath,
                $ManifestResourceId
            )
    }

    if ($EmbeddedBytes.Length -ne $BrokerItem.Length) {
        throw (
            'EMBEDDED_BROKER_SIZE_MISMATCH ' +
            "broker=$($BrokerItem.Length) " +
            "embedded=$($EmbeddedBytes.Length)"
        )
    }

    $Hasher = [Security.Cryptography.SHA256]::Create()

    try {
        $EmbeddedHashBytes = $Hasher.ComputeHash(
            $EmbeddedBytes
        )
    }
    finally {
        $Hasher.Dispose()
    }

    $EmbeddedHash = (
        $EmbeddedHashBytes |
        ForEach-Object {
            $_.ToString('x2')
        }
    ) -join ''

    if ($EmbeddedHash -cne $BrokerHash) {
        throw (
            'EMBEDDED_BROKER_SHA256_MISMATCH ' +
            "broker=$BrokerHash " +
            "embedded=$EmbeddedHash"
        )
    }

    $LcrVersion = (
        [Diagnostics.FileVersionInfo]::GetVersionInfo(
            $LcrPath
        )
    ).ProductVersion

    $BrokerVersion = (
        [Diagnostics.FileVersionInfo]::GetVersionInfo(
            $BrokerPath
        )
    ).ProductVersion

    if ([string]::IsNullOrWhiteSpace($LcrVersion)) {
        throw 'LCR_PRODUCT_VERSION_EMPTY'
    }

    if ([string]::IsNullOrWhiteSpace($BrokerVersion)) {
        throw 'BROKER_PRODUCT_VERSION_EMPTY'
    }

    if ($LcrVersion -cne $BrokerVersion) {
        throw (
            'BROKER_HELPER_PRODUCT_VERSION_MISMATCH ' +
            "lcr=$LcrVersion broker=$BrokerVersion"
        )
    }
    if ($SignedBroker) {
        if ($CanonicalVersion -cne $LcrVersion -or
            $CanonicalVersion -cne $BrokerVersion) {
            throw (
                'SIGNED_BROKER_CANONICAL_VERSION_MISMATCH ' +
                "canonical=$CanonicalVersion " +
                "lcr=$LcrVersion " +
                "broker=$BrokerVersion"
            )
        }

        $BrokerHashAfter = (
            Get-FileHash `
                -LiteralPath $BrokerPath `
                -Algorithm SHA256
        ).Hash.ToLowerInvariant()

        if ($BrokerHashAfter -cne $BrokerHash) {
            throw (
                'SIGNED_BROKER_CHANGED_AFTER_SIGNING ' +
                "before=$BrokerHash after=$BrokerHashAfter"
            )
        }

        [byte[]]$ExpectedManifest =
            [IO.File]::ReadAllBytes($ManifestPath)

        if (-not (
            Test-ByteArrayEqual `
                -Left $EmbeddedManifestBytes `
                -Right $ExpectedManifest
        )) {
            throw 'SIGNED_MANIFEST_RCDATA_BYTE_MISMATCH'
        }

        if ($EmbeddedManifestBytes.Length -lt 117) {
            throw (
                'SIGNED_MANIFEST_TOO_SHORT=' +
                $EmbeddedManifestBytes.Length
            )
        }

        $ManifestMagic =
            [Text.Encoding]::ASCII.GetString(
                $EmbeddedManifestBytes,
                0,
                8
            )

        if ($ManifestMagic -cne 'LCRBSIG1') {
            throw "SIGNED_MANIFEST_MAGIC=$ManifestMagic"
        }

        $ManifestFormat =
            [BitConverter]::ToUInt16(
                $EmbeddedManifestBytes,
                8
            )

        if ($ManifestFormat -ne 1) {
            throw "SIGNED_MANIFEST_FORMAT=$ManifestFormat"
        }

        $ManifestVersionLength =
            [BitConverter]::ToUInt16(
                $EmbeddedManifestBytes,
                10
            )

        if ($ManifestVersionLength -eq 0 -or
            $ManifestVersionLength -gt 128) {
            throw (
                'SIGNED_MANIFEST_VERSION_BYTES=' +
                $ManifestVersionLength
            )
        }

        $ManifestSignatureOffset =
            52 + $ManifestVersionLength

        $ManifestExpectedLength =
            $ManifestSignatureOffset + 64

        if ($EmbeddedManifestBytes.Length -ne
            $ManifestExpectedLength) {
            throw (
                'SIGNED_MANIFEST_EXACT_LENGTH_MISMATCH ' +
                "expected=$ManifestExpectedLength " +
                "actual=$($EmbeddedManifestBytes.Length)"
            )
        }

        $ManifestPayloadLength =
            [BitConverter]::ToUInt64(
                $EmbeddedManifestBytes,
                12
            )

        if ($ManifestPayloadLength -ne
            [UInt64]$EmbeddedBytes.Length) {
            throw (
                'SIGNED_MANIFEST_PAYLOAD_LENGTH_MISMATCH ' +
                "declared=$ManifestPayloadLength " +
                "actual=$($EmbeddedBytes.Length)"
            )
        }

        [byte[]]$ManifestPayloadHash =
            New-Object byte[] 32

        [Array]::Copy(
            $EmbeddedManifestBytes,
            20,
            $ManifestPayloadHash,
            0,
            32
        )

        if (-not (
            Test-ByteArrayEqual `
                -Left $ManifestPayloadHash `
                -Right $EmbeddedHashBytes
        )) {
            throw 'SIGNED_MANIFEST_PAYLOAD_SHA256_MISMATCH'
        }

        $ManifestVersion =
            [Text.Encoding]::UTF8.GetString(
                $EmbeddedManifestBytes,
                52,
                $ManifestVersionLength
            )

        if ($ManifestVersion -cne $CanonicalVersion) {
            throw (
                'SIGNED_MANIFEST_VERSION_MISMATCH ' +
                "expected=$CanonicalVersion " +
                "actual=$ManifestVersion"
            )
        }

        [byte[]]$EmbeddedSignedMessage =
            New-Object byte[] $ManifestSignatureOffset

        [Array]::Copy(
            $EmbeddedManifestBytes,
            0,
            $EmbeddedSignedMessage,
            0,
            $ManifestSignatureOffset
        )

        [byte[]]$EmbeddedSignature =
            New-Object byte[] 64

        [Array]::Copy(
            $EmbeddedManifestBytes,
            $ManifestSignatureOffset,
            $EmbeddedSignature,
            0,
            64
        )

        [IO.File]::WriteAllBytes(
            $EmbeddedMessagePath,
            $EmbeddedSignedMessage
        )

        [IO.File]::WriteAllBytes(
            $EmbeddedSignaturePath,
            $EmbeddedSignature
        )

        & $OpenSsl `
            pkeyutl `
            -verify `
            -rawin `
            -pubin `
            -keyform DER `
            -inkey $PinnedPublicDerPath `
            -in $EmbeddedMessagePath `
            -sigfile $EmbeddedSignaturePath `
            *> $null

        if ($LASTEXITCODE -ne 0) {
            throw 'SIGNED_MANIFEST_RCDATA_SIGNATURE_INVALID'
        }

        $ManifestHash = (
            Get-FileHash `
                -LiteralPath $ManifestPath `
                -Algorithm SHA256
        ).Hash.ToLowerInvariant()

        Write-Host "SIGNED_MANIFEST_BYTES=$($EmbeddedManifestBytes.Length)"
        Write-Host "SIGNED_MANIFEST_SHA256=$ManifestHash"
        Write-Host 'SIGNED_BROKER_PRIVATE_KEY_IDENTITY=PINNED_MATCH'
        Write-Host 'SIGNED_BROKER_PREBUNDLE_SIGNATURE=PASS'
        Write-Host 'SIGNED_MANIFEST_RCDATA=PASS'
        Write-Host 'SIGNED_MANIFEST_EXACT_LENGTH=PASS'
        Write-Host 'SIGNED_MANIFEST_VERSION_BINDING=PASS'
        Write-Host 'SIGNED_MANIFEST_PAYLOAD_LENGTH_BINDING=PASS'
        Write-Host 'SIGNED_MANIFEST_PAYLOAD_SHA256_BINDING=PASS'
        Write-Host 'PINNED_PUBLIC_KEY_SIGNATURE=PASS'
        Write-Host 'SIGNED_CANONICAL_BUNDLE=PASS'
    }

    $LcrItem = Get-Item -LiteralPath $LcrPath
    $LcrHash = (
        Get-FileHash `
            -LiteralPath $LcrPath `
            -Algorithm SHA256
    ).Hash.ToLowerInvariant()

    Write-Host "LCR_RELEASE_PATH=$LcrPath"
    Write-Host "LCR_RELEASE_BYTES=$($LcrItem.Length)"
    Write-Host "LCR_RELEASE_SHA256=$LcrHash"

    Write-Host "BROKER_RELEASE_PATH=$BrokerPath"
    Write-Host "BROKER_RELEASE_BYTES=$($BrokerItem.Length)"
    Write-Host "BROKER_RELEASE_SHA256=$BrokerHash"

    Write-Host "EMBEDDED_BROKER_BYTES=$($EmbeddedBytes.Length)"
    Write-Host "EMBEDDED_BROKER_SHA256=$EmbeddedHash"

    Write-Host "LCR_PRODUCT_VERSION=$LcrVersion"
    Write-Host "BROKER_PRODUCT_VERSION=$BrokerVersion"

    Write-Host 'EXACT_BROKER_RCDATA=PASS'
    Write-Host 'BROKER_HELPER_VERSION_BINDING=PASS'

    if ($SignedBroker) {
        Write-Host 'SIGNED_BROKER_MODE=YES'
        Write-Host 'BROKER_SIGNED_MANIFEST_RESOURCE_ID=102'
    }
    else {
        Write-Host 'SIGNED_BROKER_MODE=NO'
    }

    Write-Host 'CANONICAL_BUNDLED_RELEASE=PASS'
}
finally {
    foreach ($Temporary in $SignedTemporaryPaths) {
        Remove-Item `
            -LiteralPath $Temporary `
            -Force `
            -ErrorAction SilentlyContinue
    }

    if ($null -eq $OldEmbed) {
        Remove-Item `
            -LiteralPath "Env:$EmbedEnv" `
            -ErrorAction SilentlyContinue
    }
    else {
        Set-Item `
            -LiteralPath "Env:$EmbedEnv" `
            -Value $OldEmbed
    }

    if ($null -eq $OldManifestEmbed) {
        Remove-Item `
            -LiteralPath "Env:$ManifestEmbedEnv" `
            -ErrorAction SilentlyContinue
    }
    else {
        Set-Item `
            -LiteralPath "Env:$ManifestEmbedEnv" `
            -Value $OldManifestEmbed
    }
}