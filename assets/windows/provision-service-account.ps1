#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Account
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    [Console]::Error.WriteLine((@{ ok = $false; account = $Account; error = 'Windows is required' } | ConvertTo-Json -Compress))
    exit 1
}

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

internal static class RoostLsaRights
{
    private const uint POLICY_CREATE_ACCOUNT = 0x00000010;
    private const uint POLICY_LOOKUP_NAMES = 0x00000800;
    private const int ERROR_INSUFFICIENT_BUFFER = 122;

    [StructLayout(LayoutKind.Sequential)]
    private struct LSA_OBJECT_ATTRIBUTES
    {
        public uint Length;
        public IntPtr RootDirectory;
        public IntPtr ObjectName;
        public uint Attributes;
        public IntPtr SecurityDescriptor;
        public IntPtr SecurityQualityOfService;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct LSA_UNICODE_STRING
    {
        public ushort Length;
        public ushort MaximumLength;
        public IntPtr Buffer;
    }

    private enum SID_NAME_USE
    {
        SidTypeUser = 1,
        SidTypeGroup,
        SidTypeDomain,
        SidTypeAlias,
        SidTypeWellKnownGroup,
        SidTypeDeletedAccount,
        SidTypeInvalid,
        SidTypeUnknown,
        SidTypeComputer,
        SidTypeLabel,
        SidTypeLogonSession
    }

    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool LookupAccountName(
        string systemName,
        string accountName,
        IntPtr sid,
        ref uint sidSize,
        StringBuilder referencedDomainName,
        ref uint referencedDomainNameSize,
        out SID_NAME_USE use);

    [DllImport("advapi32.dll")]
    private static extern uint LsaOpenPolicy(
        IntPtr systemName,
        ref LSA_OBJECT_ATTRIBUTES objectAttributes,
        uint desiredAccess,
        out IntPtr policyHandle);

    [DllImport("advapi32.dll")]
    private static extern uint LsaAddAccountRights(
        IntPtr policyHandle,
        IntPtr accountSid,
        LSA_UNICODE_STRING[] userRights,
        uint countOfRights);

    [DllImport("advapi32.dll")]
    private static extern uint LsaNtStatusToWinError(uint status);

    [DllImport("advapi32.dll")]
    private static extern uint LsaClose(IntPtr policyHandle);

    private static void ThrowLsa(uint status, string operation)
    {
        if (status == 0) return;
        throw new Win32Exception((int)LsaNtStatusToWinError(status), operation);
    }

    public static void GrantServiceLogon(string accountName)
    {
        uint sidSize = 0;
        uint domainSize = 0;
        SID_NAME_USE use;
        LookupAccountName(null, accountName, IntPtr.Zero, ref sidSize, null, ref domainSize, out use);
        int firstError = Marshal.GetLastWin32Error();
        if (firstError != ERROR_INSUFFICIENT_BUFFER || sidSize == 0)
            throw new Win32Exception(firstError, "LookupAccountName size query failed");

        IntPtr sid = Marshal.AllocHGlobal(checked((int)sidSize));
        IntPtr rightBuffer = IntPtr.Zero;
        IntPtr policy = IntPtr.Zero;
        try
        {
            var domain = new StringBuilder(checked((int)domainSize));
            if (!LookupAccountName(null, accountName, sid, ref sidSize, domain, ref domainSize, out use))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "LookupAccountName failed");

            var attributes = new LSA_OBJECT_ATTRIBUTES();
            attributes.Length = (uint)Marshal.SizeOf(typeof(LSA_OBJECT_ATTRIBUTES));
            ThrowLsa(
                LsaOpenPolicy(IntPtr.Zero, ref attributes, POLICY_LOOKUP_NAMES | POLICY_CREATE_ACCOUNT, out policy),
                "LsaOpenPolicy failed");

            const string right = "SeServiceLogonRight";
            rightBuffer = Marshal.StringToHGlobalUni(right);
            var rights = new[] {
                new LSA_UNICODE_STRING {
                    Buffer = rightBuffer,
                    Length = checked((ushort)(right.Length * 2)),
                    MaximumLength = checked((ushort)((right.Length + 1) * 2))
                }
            };
            ThrowLsa(LsaAddAccountRights(policy, sid, rights, 1), "LsaAddAccountRights failed");
        }
        finally
        {
            if (policy != IntPtr.Zero) LsaClose(policy);
            if (rightBuffer != IntPtr.Zero) Marshal.FreeHGlobal(rightBuffer);
            Marshal.FreeHGlobal(sid);
        }
    }
}
'@

try {
    [RoostLsaRights]::GrantServiceLogon($Account)
    @{ ok = $true; account = $Account; right = 'SeServiceLogonRight' } | ConvertTo-Json -Compress
} catch {
    [Console]::Error.WriteLine((@{
        ok = $false
        account = $Account
        right = 'SeServiceLogonRight'
        error = $_.Exception.Message
    } | ConvertTo-Json -Compress))
    exit 1
}
