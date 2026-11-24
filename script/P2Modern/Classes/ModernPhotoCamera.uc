// Carries photo mode's view. Hidden, touching nothing, moved only by ModernPhotoMode and gone when it closes.
class ModernPhotoCamera extends Actor
	notplaceable
	transient;

defaultproperties
{
	bHidden=True
	bCollideActors=False
	bCollideWorld=False
	bBlockActors=False
	bBlockPlayers=False
	bProjTarget=False
	bGameRelevant=True
	RemoteRole=ROLE_None
	Physics=PHYS_None
}
